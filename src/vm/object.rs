// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Memory objects' kernel side.

use core::mem::{self, ManuallyDrop};
use core::ops::{Add, Range};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use intrusive_collections::{
    Bound, LinkedList, LinkedListAtomicLink, RBTree, UnsafeRef, intrusive_adapter,
};

use super::STATISTICS;
use super::page::{self, Page, PageTreeAdapter};
use crate::arch::{MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE};
use crate::device::pager::{self, Pager};
use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::reference::{Count, Counted, Reference};
use crate::slab::{Cache, CacheBox};
use crate::sync::{Token, TokenGuard};

/// The kernel's side of a memory object; so far only its references and
/// name port, its place in the list of persisting objects and its state.
pub struct Object {
    /// How many references it has.
    references: Count,
    /// Its name port, which names it to `vm_region`, as a reference given
    /// up by `into_raw`; null until it has one.
    pager_name: AtomicPtr<Port>,
    /// Its place in the list of persisting objects.
    persisting_link: LinkedListAtomicLink,
    /// Its state, behind its token.
    state: Token<ObjectState>,
}

/// A place in an object, in bytes from its start.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Offset(usize);

impl From<usize> for Offset {
    fn from(offset: usize) -> Self {
        Self(offset)
    }
}

impl From<Offset> for usize {
    fn from(offset: Offset) -> Self {
        offset.0
    }
}

impl Add<usize> for Offset {
    type Output = Self;

    fn add(self, size: usize) -> Self {
        Self(self.0 + size)
    }
}

/// What an object's token guards; so far only its size, its pages, where
/// the last one went in, its paging under way, its pager and its first
/// values.
pub struct ObjectState {
    /// Its size, in bytes.
    size: usize,
    /// Its resident pages.
    pages: Pages,
    /// The offset its last page went in at.
    last_alloc: usize,
    /// The object its pages are copied into before they change; none yet.
    pub(super) copy: Option<Reference<Object>>,
    /// The object behind it, whose pages show where it has none.
    pub(super) shadow: Option<Reference<Object>>,
    /// Where it starts in its shadow.
    pub(super) shadow_offset: Offset,
    /// How many paging operations on it are under way.
    pub(super) paging_in_progress: u32,
    /// Its `Flags`.
    pub(crate) flags: Flags,
    /// What serves its pages; none until a pager is entered for it.
    pub(super) kind: Option<Kind>,
    /// A send right for its pager, the memory object; none until it has one.
    pager: Option<Reference<Port>>,
    /// Its request port, which its pager calls the kernel on; none until it
    /// has a pager.
    pager_request: Option<Reference<Port>>,
    /// How its data is copied.
    copy_strategy: CopyStrategy,
}

/// What serves an object's pages; only the kinds used here.
pub(super) enum Kind {
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
struct Pages {
    /// The pages, by offset.
    tree: RBTree<PageTreeAdapter>,
    /// How many there are.
    count: usize,
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

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    PersistingAdapter = UnsafeRef<Object>: Object { persisting_link => LinkedListAtomicLink }
);

impl Object {
    /// A new object of `size` bytes, with one reference.
    const fn new(size: usize) -> Self {
        Self {
            references: Count::new(1),
            pager_name: AtomicPtr::new(ptr::null_mut()),
            persisting_link: LinkedListAtomicLink::new(),
            state: Token::new(ObjectState {
                size,
                pages: Pages {
                    tree: RBTree::new(PageTreeAdapter::NEW),
                    count: 0,
                },
                last_alloc: 0,
                copy: None,
                shadow: None,
                shadow_offset: Offset(0),
                paging_in_progress: 0,
                flags: Flags::INTERNAL.union(Flags::TEMPORARY).union(Flags::ALIVE),
                kind: None,
                pager: None,
                pager_request: None,
                copy_strategy: CopyStrategy::None,
            }),
        }
    }

    /// How many references it has.
    pub(super) fn references(&self) -> u32 {
        self.references.get()
    }

    /// Takes its token.
    pub fn lock(&self) -> TokenGuard<'_, ObjectState> {
        self.state.lock()
    }
}

// SAFETY: its count is its own, and every object but the kernel object,
// whose own reference never drops, is a box of `CACHE`, which `free` takes
// back.
unsafe impl Counted for Object {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Terminates `object`, as `vm_object_deallocate` does at its last
    /// reference.
    ///
    /// # Panics
    ///
    /// If it may persist with pages, or its pager is not yet initialized:
    /// not yet. As `terminate`.
    unsafe fn free(object: NonNull<Self>) {
        // SAFETY: nothing else uses it, and it stays allocated until
        // `terminate` frees it.
        let state = unsafe { object.as_ref() }.lock();
        assert!(
            !state.flags.contains(Flags::CAN_PERSIST) || state.pages.count == 0,
            "vm_object_deallocate: vm_object_cache_add, not yet"
        );
        assert!(
            !state.flags.contains(Flags::PAGER_CREATED)
                || state.flags.contains(Flags::PAGER_INITIALIZED),
            "vm_object_deallocate: waiting for its pager's initialization, not yet"
        );
        // SAFETY: the caller's contract, and `state` is its state.
        unsafe { terminate(object, state) };
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
    pub(super) const fn end_paging(&mut self) {
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
    pub(super) fn insert(&mut self, object: &Object, page: &'static Page, offset: usize) {
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
    pub(super) fn lookup(&self, offset: usize) -> Option<&'static Page> {
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
    pub(super) fn collapse(state: &mut TokenGuard<'_, Self>, object: &Object) {
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
                            page::free(page);
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
    pub(super) fn remove_pages(&mut self, range: Range<usize>) {
        let mut cursor = self
            .pages
            .tree
            .lower_bound_mut(Bound::Included(&range.start));
        while cursor.get().is_some_and(|page| page.offset() < range.end) {
            // vm_page_remove: out of the object and off its paging queue.
            let page = cursor.remove().unwrap();
            self.pages.count -= 1;
            page.remove_from_queue();
            page::free(page);
        }
    }
}

/// The object all wired kernel memory belongs to, at the offset of its
/// address in the kernel map.
pub static KERNEL_OBJECT: Object = Object::new(MAX_KERNEL_ADDRESS - MIN_KERNEL_ADDRESS + 1);

/// The unreferenced objects kept in case they are used again, until memory
/// runs low.
#[expect(dead_code, reason = "vm_object_deallocate and vm_object_lookup use it")]
static PERSISTING: LinkedList<PersistingAdapter> = LinkedList::new(PersistingAdapter::NEW);

/// The cache objects come from.
static CACHE: Cache<Object> = Cache::new("vm_object");

/// How many objects pagers outside the kernel's default one manage, as
/// `vm_statistics` reports them.
static EXTERNAL_COUNT: AtomicU32 = AtomicU32::new(0);

/// Sets up memory objects, on the boot CPU.
pub fn bootstrap() {
    CACHE.register();
}

/// Gives the kernel object its name port, on the boot CPU.
///
/// # Panics
///
/// If no port can be allocated.
pub fn init() {
    let port = Port::allocate_kernel()
        .unwrap_or_else(|error| panic!("vm_object_init: ipc_port_alloc_kernel failed: {error:?}"));
    port.set_kernel_object(KernelObject::PagerName(NonNull::from(&KERNEL_OBJECT)));
    // The slot keeps its reference.
    KERNEL_OBJECT.pager_name.store(
        Reference::into_raw(Reference::from(port)).as_ptr(),
        Ordering::Release,
    );
}

/// A new internal, temporary object of `size` bytes, with one reference
/// and a name port.
///
/// # Panics
///
/// If the object or its port can't be allocated.
pub fn allocate(size: usize) -> Reference<Object> {
    let object = CACHE
        .allocate(Object::new(size))
        .unwrap_or_else(|error| panic!("vm_object_allocate: {error:?}"));
    // SAFETY: a new object's box holds its first reference, given up here.
    let object = unsafe { Reference::from_raw(CacheBox::into_raw(object)) };
    let port =
        Port::allocate_kernel().unwrap_or_else(|error| panic!("vm_object_allocate: {error:?}"));
    port.set_kernel_object(KernelObject::PagerName(Reference::as_ptr(&object)));
    // The slot keeps its reference.
    object.pager_name.store(
        Reference::into_raw(Reference::from(port)).as_ptr(),
        Ordering::Release,
    );
    object
}

/// Another reference to `object` for a copy that shares it, as
/// `vm_object_copy_temporary` makes one for a temporary object: the source
/// and the copy each shadow it before writing; none if it isn't temporary.
pub fn copy_temporary(object: &Reference<Object>) -> Option<Reference<Object>> {
    let mut state = object.lock();
    if !state.flags.contains(Flags::TEMPORARY) {
        drop(state);
        return None;
    }
    // Symmetric: GNU Mach's use_shared_copy, for a delayed copy, is set only
    // by vm_map_fork.
    let copy = Reference::clone(object);
    state.flags.insert(Flags::SHADOWED);
    drop(state);
    Some(copy)
}

/// A new internal, temporary object of `size` bytes, with one reference
/// and a name port, in front of `source` from `offset`, as
/// `vm_object_shadow` makes it: its pages show where it has none. It takes
/// `source`'s reference.
///
/// # Panics
///
/// As `allocate`.
pub fn shadow(source: Reference<Object>, offset: Offset, size: usize) -> Reference<Object> {
    let object = allocate(size);
    let mut state = object.lock();
    // The reference moves: `source`'s count stays.
    state.shadow = Some(source);
    state.shadow_offset = offset;
    drop(state);
    object
}

/// The object and offset two adjoining ranges, `previous_size` bytes of
/// `previous` from `previous_offset` and `next_size` bytes of `next` from
/// `next_offset`, none for no object, have once joined, with a reference
/// to that object for the range they make, as `vm_object_coalesce` gives
/// them; none if they can't join.
///
/// # Panics
///
/// If they are the same object and contiguous, or only `next` has one and it
/// would take the range before it: not yet. As `ObjectState::collapse`.
pub fn coalesce(
    previous: Option<&Reference<Object>>,
    next: Option<&Reference<Object>>,
    previous_offset: Offset,
    next_offset: Offset,
    previous_size: usize,
    next_size: usize,
) -> Option<(Option<Reference<Object>>, Offset)> {
    let object = match (previous, next) {
        // Neither exists, so the offsets don't matter.
        (None, None) => return Some((None, Offset::from(0))),
        (Some(previous), Some(next)) if Reference::as_ptr(previous) == Reference::as_ptr(next) => {
            assert!(
                previous_offset + previous_size != next_offset,
                "vm_object_coalesce: the same object, not yet"
            );
            return None;
        }
        // Two different objects don't merge.
        (Some(_), Some(_)) => return None,
        (Some(object), None) | (None, Some(object)) => object,
    };
    let mut state = object.lock();
    ObjectState::collapse(&mut state, object);
    // Refused if pages outside these ranges may be in use.
    if object.references() > 1
        || state.flags.contains(Flags::PAGER_CREATED)
        || state.shadow.is_some()
        || state.copy.is_some()
        || state.paging_in_progress != 0
    {
        return None;
    }
    assert!(
        previous.is_some(),
        "vm_object_coalesce: the next object, not yet"
    );
    // Pages left from an earlier deallocation go, then it grows over the
    // range.
    let end = usize::from(previous_offset + previous_size);
    state.remove_pages(end..end + next_size);
    state.size = state.size.max(end + next_size);
    drop(state);
    Some((Some(object.clone()), previous_offset))
}

/// The object of the memory object `pager`, of `size` bytes, made and set
/// up with its pager, as `vm_object_enter` gives it for an object outside
/// the kernel, with a reference for the caller; none if `pager` stands for
/// something else.
///
/// # Panics
///
/// If `pager` has an object, another call made one meanwhile, or `pager`
/// is a user's: not yet. If no request port can be allocated. As
/// `allocate` and `pager::init_pager`.
pub fn enter(pager: &Reference<Port>, size: usize) -> Option<Reference<Object>> {
    // A device pager's record, or none for a user's pager.
    let record = match pager.kernel_object() {
        Some(KernelObject::DevicePager(record)) => Some(record),
        None => None,
        Some(KernelObject::Pager(_)) => panic!("vm_object_enter: an existing object, not yet"),
        Some(_) => {
            STATISTICS.lookups.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    // Made first; setting it re-checks the port.
    let object = allocate(size);
    assert!(
        matches!(
            pager.replace_kernel_object(Some(KernelObject::Pager(Reference::as_ptr(&object)))),
            Some(KernelObject::DevicePager(_)) | None
        ),
        "vm_object_enter: an object made meanwhile, not yet"
    );
    STATISTICS.lookups.fetch_add(1, Ordering::Relaxed);
    let pager = Port::copy_send(pager);
    let request = Reference::from(
        Port::allocate_kernel()
            .unwrap_or_else(|error| panic!("vm_object_enter: pager request alloc: {error:?}")),
    );
    request.set_kernel_object(KernelObject::PagingRequest(Reference::as_ptr(&object)));
    let mut state = object.lock();
    state.flags.insert(Flags::PAGER_CREATED);
    state.pager = Some(pager);
    state.pager_request = Some(request);
    // External, not temporary, and not ready until its pager says so.
    state.flags.remove(Flags::INTERNAL | Flags::TEMPORARY);
    // SAFETY: a pager's record stays allocated while its device lists it,
    // as long as its object lives.
    state.kind = record.map(|record| Kind::Device(unsafe { UnsafeRef::from_raw(record.as_ptr()) }));
    drop(state);
    EXTERNAL_COUNT.fetch_add(1, Ordering::Relaxed);
    // memory_object_init: its rights, made from the request and name ports,
    // then the kernel's own pager called directly.
    let request_send = Port::make_send(object.lock().pager_request.as_ref().unwrap());
    // SAFETY: the object's slot holds a reference to its name port, which
    // this handle borrows and never drops.
    let name = ManuallyDrop::new(unsafe {
        Reference::from_raw(NonNull::new(object.pager_name.load(Ordering::Acquire)).unwrap())
    });
    let name_send = Port::make_send(&name);
    let record = record.unwrap_or_else(|| panic!("memory_object_init: a user pager, not yet"));
    // SAFETY: as for its kind above.
    pager::init_pager(unsafe { record.as_ref() }, request_send, name_send);
    object.lock().flags.insert(Flags::PAGER_INITIALIZED);
    // Nothing waits for it to be initialized yet, so none is woken.
    Some(object)
}

/// Readies the object whose request port `request` is, as its pager's
/// `memory_object_ready` asks, with `memory_object_set_attributes_common`:
/// not to persist, copied as `copy_strategy` says.
///
/// # Panics
///
/// If `request` is no object's request port: not yet. As `lookup` and, at
/// the object's last reference, `terminate`.
pub fn ready(request: &Port, copy_strategy: CopyStrategy) {
    let object = request
        .to_vm_object()
        .unwrap_or_else(|| panic!("memory_object_ready: KERN_INVALID_ARGUMENT, not yet"));
    let mut state = object.lock();
    // Nothing waits for it to be ready yet, so none is woken.
    state.flags.remove(Flags::CAN_PERSIST);
    state.flags.insert(Flags::PAGER_READY);
    state.copy_strategy = copy_strategy;
    drop(state);
    // vm_object_deallocate.
    drop(object);
}

/// Terminates `object`, whose last reference dropped, `state` its state
/// locked, as `vm_object_terminate` does: dead, unknown to its ports, then,
/// unlocked, its pager terminated with the rights it held, else its name
/// port destroyed, and freed.
///
/// # Safety
///
/// `object` is a box of `CACHE` given up by `into_raw`, which nothing uses
/// after.
///
/// # Panics
///
/// If it is dead, or one of its ports stands for something else. If it has
/// a shadow, paging under way or pages, or a user's pager: not yet. As
/// `pager::terminate` and `Port::deallocate_kernel`.
unsafe fn terminate(object: NonNull<Object>, mut state: TokenGuard<'_, ObjectState>) {
    assert!(
        state.flags.contains(Flags::ALIVE),
        "vm_object_terminate: a dead object"
    );
    state.flags.remove(Flags::ALIVE);
    // vm_object_remove: no lookup finds it now. Its pager's port stands for
    // nothing in place of IKOT_PAGER_TERMINATING; vm_object_enter's wait on
    // it is not yet.
    if let Some(pager) = &state.pager {
        assert!(
            matches!(
                pager.replace_kernel_object(None),
                Some(KernelObject::Pager(_)) | None
            ),
            "vm_object_remove: bad object port"
        );
    }
    if let Some(request) = &state.pager_request {
        assert!(
            matches!(
                request.replace_kernel_object(None),
                Some(KernelObject::PagingRequest(_)) | None
            ),
            "vm_object_remove: bad request port"
        );
    }
    // SAFETY: the caller's contract.
    let pager_name = &unsafe { object.as_ref() }.pager_name;
    // SAFETY: the slot holds its port's reference, borrowed here.
    if let Some(name) = unsafe { pager_name.load(Ordering::Acquire).as_ref() } {
        assert!(
            matches!(
                name.replace_kernel_object(None),
                Some(KernelObject::PagerName(_)) | None
            ),
            "vm_object_remove: bad name port"
        );
    }
    assert!(
        state.shadow.is_none(),
        "vm_object_terminate: a shadow object, not yet"
    );
    assert!(
        state.paging_in_progress == 0,
        "vm_object_terminate: vm_object_paging_wait, not yet"
    );
    assert!(
        state.pages.count == 0,
        "vm_object_terminate: freeing its pages, not yet"
    );
    if !state.flags.contains(Flags::INTERNAL) {
        EXTERNAL_COUNT.fetch_sub(1, Ordering::Relaxed);
    }
    let (kind, pager, request) = (
        state.kind.take(),
        state.pager.take(),
        state.pager_request.take(),
    );
    drop(state);
    // SAFETY: the slot held its port's reference, taken here.
    let name = NonNull::new(pager_name.swap(ptr::null_mut(), Ordering::Acquire))
        .map(|name| unsafe { Reference::from_raw(name) });
    let Some(pager) = pager else {
        // Its name port's right, which ipc_port_dealloc_kernel consumes.
        if let Some(name) = name {
            name.deallocate_kernel();
            drop(name);
        }
        // SAFETY: the caller's contract.
        drop(unsafe { CacheBox::from_raw(&CACHE, object) });
        return;
    };
    let Some(Kind::Device(record)) = kind else {
        panic!("memory_object_terminate: a user pager, not yet");
    };
    // memory_object_release: memory_object_terminate, served by the kernel's
    // own pager, called directly; nothing waits for the pager's port.
    // SAFETY: the object served by the record is terminating.
    unsafe {
        pager::terminate(
            record,
            &pager,
            request.expect("device_pager_terminate: no request port"),
            name.expect("device_pager_terminate: no name port"),
        );
    }
    // The request's destination right, as ipc_kobject_server releases it.
    Port::release_send(pager);
    // SAFETY: the caller's contract.
    drop(unsafe { CacheBox::from_raw(&CACHE, object) });
}
