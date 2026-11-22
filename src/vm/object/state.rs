// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem;
use core::ops::Range;
use core::ptr::{self, NonNull};
use core::sync::atomic::Ordering;

use intrusive_collections::{Bound, LinkedList, RBTree, UnsafeRef};

use super::object::{CACHE, Object, PersistingAdapter};
use super::offset::Offset;
use crate::arch::PAGE_SIZE;
use crate::device::pager::Pager;
use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::slab::CacheBox;
use crate::sync::TokenGuard;
use crate::vm::page::{self, page::Page, page::PageTreeAdapter};

/// What an object's token guards; so far only its size, its pages, where
/// the last one went in, its paging under way, its pager and its first
/// values.
pub struct ObjectState {
    /// Its size, in bytes.
    pub(super) size: usize,
    /// Its resident pages.
    pub(super) pages: Pages,
    /// The offset its last page went in at.
    pub(super) last_alloc: usize,
    /// The object its pages are copied into before they change; none yet.
    pub(in crate::vm) copy: Option<Reference<Object>>,
    /// The object behind it, whose pages show where it has none.
    pub(in crate::vm) shadow: Option<Reference<Object>>,
    /// Where it starts in its shadow.
    pub(in crate::vm) shadow_offset: Offset,
    /// How many paging operations on it are under way.
    pub(in crate::vm) paging_in_progress: u32,
    /// Its `Flags`.
    pub(crate) flags: Flags,
    /// What serves its pages; none until a pager is entered for it.
    pub(in crate::vm) kind: Option<Kind>,
    /// A send right for its pager, the memory object; none until it has one.
    pub(super) pager: Option<Reference<Port>>,
    /// Its request port, which its pager calls the kernel on; none until it
    /// has a pager.
    pub(super) pager_request: Option<Reference<Port>>,
    /// How its data is copied.
    pub(super) copy_strategy: CopyStrategy,
}

/// What serves an object's pages; only the kinds used here.
pub(in crate::vm) enum Kind {
    /// A device's memory, through its pager's record.
    Device(UnsafeRef<Pager>),
}

/// How an object's data is copied, as Mach's
/// `memory_object_copy_strategy_t` names it; only the values used here.
pub enum CopyStrategy {
    /// `MEMORY_OBJECT_COPY_NONE`: by reading it all, with no help.
    None,
}

/// An object's resident pages.
pub(super) struct Pages {
    /// The pages, by offset.
    pub(super) tree: RBTree<PageTreeAdapter>,
    /// How many there are.
    pub(super) count: usize,
}

bitflags::bitflags! {
    /// Whether an object had a pager, how it is managed and whether it lives.
    pub(crate) struct Flags: u8 {
        /// A pager was ever made for it.
        const PAGER_CREATED = 1;
        /// The kernel created it, so the default memory manager manages it.
        const INTERNAL = 1 << 1;
        /// Changes stay in memory rather than going back to its manager.
        const TEMPORARY = 1 << 2;
        /// Not yet terminated.
        const ALIVE = 1 << 3;
        /// Its pager was set up.
        const PAGER_INITIALIZED = 1 << 4;
        /// Its pager said it is ready.
        const PAGER_READY = 1 << 5;
        /// It stays, unreferenced, among the persisting objects.
        const CAN_PERSIST = 1 << 6;
        /// Shadows of it may exist.
        const SHADOWED = 1 << 7;
    }
}

impl ObjectState {
    /// Notes a paging operation under way.
    pub const fn begin_paging(&mut self) {
        self.paging_in_progress += 1;
    }

    /// Notes the end of a paging operation under way.
    ///
    /// # Panics
    ///
    /// If none is under way.
    pub(in crate::vm) const fn end_paging(&mut self) {
        assert!(self.paging_in_progress != 0, "vm_object_paging_end");
        self.paging_in_progress -= 1;
        // Nothing waits for paging to end yet, so none is woken.
    }

    /// Puts `page`, which belongs to no object, in `object`, whose state
    /// this is, at `offset`; the page put in last, if just before it and
    /// not busy, is deactivated.
    ///
    /// # Panics
    ///
    /// If `page` belongs to an object, or as `Page::deactivate`.
    pub(in crate::vm) fn insert(&mut self, object: &Object, page: &'static Page, offset: usize) {
        page.set_object(object, offset);
        self.pages.tree.insert(page);
        self.pages.count += 1;
        // Sequential access: the page before is no longer interesting.
        if offset == self.last_alloc + PAGE_SIZE
            && let Some(last) = self.lookup(self.last_alloc)
            && !last.is_busy()
        {
            last.deactivate();
        }
        self.last_alloc = offset;
    }

    /// The page at `offset`; `None` if none is resident.
    pub(in crate::vm) fn lookup(&self, offset: usize) -> Option<&'static Page> {
        self.pages.tree.find(&offset).clone_pointer()
    }

    /// Collapses `object`, whose state `state` is, with its shadow, as
    /// `vm_object_collapse` does: merges in a shadow it holds the only
    /// reference to, or bypasses one none of whose pages shows through it,
    /// unlocking `state` to free the merged shadow or drop the bypassed
    /// one's reference. Nothing if `object` had a pager, has paging under
    /// way or has no shadow, or its shadow is external, has paging under
    /// way, or, to be bypassed, had a pager or shows a page.
    ///
    /// # Panics
    ///
    /// If the shadow's token is held, the shadow has a shadow, or a merged
    /// shadow had a pager or a page to move up: not yet. At a bypassed
    /// shadow's last reference, as its free. As `Port::deallocate_kernel`
    /// for `object`'s old name port.
    pub(in crate::vm) fn collapse(state: &mut TokenGuard<'_, Self>, object: &Object) {
        loop {
            // No page is ever absent.
            if state.flags.contains(Flags::PAGER_CREATED) || state.paging_in_progress != 0 {
                return;
            }
            let Some(backing) = &state.shadow else {
                return;
            };
            // SAFETY: `state`, or a merge that takes it out, holds the
            // shadow's reference until after the shadow's guard drops.
            let backing: &Object = unsafe { Reference::as_ptr(backing).as_ref() };
            // Tried: waiting with `state` held is not allowed.
            let Some(mut backing_state) = backing.state.try_lock() else {
                panic!("vm_object_collapse: waiting for the backing object's lock, not yet");
            };
            if !backing_state.flags.contains(Flags::INTERNAL)
                || backing_state.paging_in_progress != 0
            {
                return;
            }
            assert!(
                backing_state.shadow.is_none(),
                "vm_object_collapse: a backing object with a shadow, not yet"
            );
            let backing_offset = usize::from(state.shadow_offset);
            let size = state.size;
            if backing.references() == 1 {
                // Its pages outside the object or under one of its pages
                // are freed; the others would move up.
                while let Some(page) = backing_state.pages.tree.front().clone_pointer() {
                    match page.offset().checked_sub(backing_offset) {
                        Some(offset) if offset < size && state.lookup(offset).is_none() => {
                            panic!("vm_object_collapse: vm_page_rename, not yet");
                        }
                        _ => {
                            // vm_page_remove: out of the object and off its
                            // paging queue.
                            backing_state.pages.tree.front_mut().remove();
                            backing_state.pages.count -= 1;
                            page.remove_from_queue();
                            page::alloc::free(page);
                        }
                    }
                }
                assert!(
                    !backing_state.flags.contains(Flags::PAGER_CREATED),
                    "vm_object_collapse: moving the backing object's pager, not yet"
                );
                // The backing object's name port names the object; the
                // object's own stands for nothing.
                let name = backing.pager_name.swap(ptr::null_mut(), Ordering::Acquire);
                // SAFETY: the slot held its port's reference, taken here.
                let old_name = NonNull::new(object.pager_name.swap(name, Ordering::AcqRel))
                    .map(|old_name| unsafe { Reference::from_raw(old_name) });
                if let Some(old_name) = &old_name {
                    old_name.replace_kernel_object(None);
                }
                // SAFETY: the slot holds its port's reference, borrowed here.
                if let Some(name) = unsafe { name.as_ref() } {
                    name.set_kernel_object(KernelObject::PagerName(NonNull::from(object)));
                }
                // It shadows what the backing object did; the backing
                // object's reference moves out.
                let merged = mem::replace(&mut state.shadow, backing_state.shadow.take()).unwrap();
                state.shadow_offset =
                    state.shadow_offset + usize::from(backing_state.shadow_offset);
                drop(backing_state);
                TokenGuard::unlocked(state, || {
                    if let Some(old_name) = old_name {
                        old_name.deallocate_kernel();
                        drop(old_name);
                    }
                    // Not terminated: it has no page, pager or shadow left.
                    // SAFETY: every object but the kernel object is a box
                    // of `CACHE` that `allocate` gave up, and with its only
                    // reference nothing else uses this one.
                    drop(unsafe { CacheBox::from_raw(&CACHE, Reference::into_raw(merged)) });
                });
                continue;
            }
            if backing_state.flags.contains(Flags::PAGER_CREATED) {
                return;
            }
            // Each backing page it could show must be covered; GNU Mach's
            // range includes its end.
            let shown = backing_offset..=backing_offset + size;
            let mut pages = backing_state
                .pages
                .tree
                .lower_bound(Bound::Included(&backing_offset));
            while let Some(page) = pages.get().filter(|page| shown.contains(&page.offset())) {
                if state.lookup(page.offset() - backing_offset).is_none() {
                    return;
                }
                pages.move_next();
            }
            // It shadows what the backing object did.
            let bypassed = mem::replace(&mut state.shadow, backing_state.shadow.clone());
            state.shadow_offset = state.shadow_offset + usize::from(backing_state.shadow_offset);
            let copy = backing_state
                .copy
                .take_if(|copy| Reference::as_ptr(copy) == NonNull::from(object));
            drop(backing_state);
            // Unlocked: counts move without tokens, so a last reference may
            // drop here.
            TokenGuard::unlocked(state, || drop((bypassed, copy)));
        }
    }

    /// Frees its pages in `range`, as `vm_object_page_remove` does; the
    /// caller has unmapped them by range (no per-page `pmap_page_protect`).
    pub(in crate::vm) fn remove_pages(&mut self, range: Range<usize>) {
        let mut cursor = self
            .pages
            .tree
            .lower_bound_mut(Bound::Included(&range.start));
        while cursor.get().is_some_and(|page| page.offset() < range.end) {
            // vm_page_remove: out of the object and off its paging queue.
            let page = cursor.remove().unwrap();
            self.pages.count -= 1;
            page.remove_from_queue();
            page::alloc::free(page);
        }
    }
}

/// The unreferenced objects kept in case they are used again, until memory
/// runs low.
#[expect(dead_code, reason = "vm_object_deallocate and vm_object_lookup use it")]
static PERSISTING: LinkedList<PersistingAdapter> = LinkedList::new(PersistingAdapter::NEW);
