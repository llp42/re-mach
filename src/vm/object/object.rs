// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::ManuallyDrop;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, Ordering};

use intrusive_collections::RBTree;
use tailq::Link;

use super::external::EXTERNAL_COUNT;
use super::offset::Offset;
use super::state::{CopyStrategy, Flags, Kind, ObjectState, Pages};
use crate::arch::{MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS};
use crate::device::pager;
use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::reference::{Count, Counted, Reference};
use crate::slab::{Cache, CacheBox};
use crate::sync::{Token, TokenGuard};
use crate::vm::page::page::PageTreeAdapter;

/// The kernel's side of a memory object; so far only its references and
/// name port, its place in the list of persisting objects and its state.
pub struct Object {
    /// How many references it has.
    references: Count,
    /// Its name port, which names it to `vm_region`, as a reference given
    /// up by `into_raw`; null until it has one.
    pub(super) pager_name: AtomicPtr<Port>,
    /// Its place in the list of persisting objects.
    persisting_link: Link,
    /// Its state, behind its token.
    pub(super) state: Token<ObjectState>,
}

tailq::adapter!(pub(super) PersistingAdapter = Object { persisting_link });

impl Object {
    /// A new object of `size` bytes, with one reference.
    pub(super) const fn new(size: usize) -> Self {
        Self {
            references: Count::new(1),
            pager_name: AtomicPtr::new(ptr::null_mut()),
            persisting_link: Link::new(),
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
    pub(in crate::vm) fn references(&self) -> u32 {
        self.references.get()
    }

    /// Takes its token.
    pub fn lock(&self) -> TokenGuard<'_, ObjectState> {
        self.state.lock()
    }

    /// Whether its pager is ready, whether it may cache, and its copy
    /// strategy's number, as `memory_object_get_attributes` gives them.
    pub fn attributes(&self) -> (bool, bool, i32) {
        let state = self.lock();
        (
            state.flags.contains(Flags::PAGER_READY),
            state.flags.contains(Flags::CAN_PERSIST),
            state.copy_strategy.number(),
        )
    }

    /// A send right for its pager's port, if it has one, as
    /// `vm_region_create_proxy` copies one.
    pub fn pager_port(&self) -> Option<Reference<Port>> {
        self.lock().pager.as_ref().map(Port::copy_send)
    }

    /// A send right for its name port, if it has one, as `vm_object_name`
    /// gives it.
    pub fn name_port(&self) -> Option<Reference<Port>> {
        let pointer = self.pager_name.load(Ordering::Acquire);
        let port = NonNull::new(pointer)?;
        // SAFETY: the slot keeps its port's reference while the object
        // lives; this handle borrows it and never drops it.
        let port = ManuallyDrop::new(unsafe { Reference::from_raw(port) });
        Some(Port::make_send(&port))
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
        let mut state = unsafe { object.as_ref() }.lock();
        assert!(
            !state.flags.contains(Flags::CAN_PERSIST) || state.pages.count == 0,
            "vm_object_deallocate: vm_object_cache_add, not yet"
        );
        assert!(
            !state.flags.contains(Flags::PAGER_CREATED)
                || state.flags.contains(Flags::PAGER_INITIALIZED),
            "vm_object_deallocate: waiting for its pager's initialization, not yet"
        );
        // Its shadow's reference leaves with it, as vm_object_deallocate
        // takes it before terminate; the shadow's copy, if this object, is
        // detached.
        let shadow = state.shadow.take();
        if let Some(shadow) = &shadow {
            let mut backing = shadow.lock();
            if backing.copy.as_ref().map(Reference::as_ptr) == Some(object) {
                backing.copy = None;
            }
            drop(backing);
        }
        // SAFETY: the caller's contract, and `state` is its state.
        unsafe { terminate(object, state) };
        // The shadow's reference goes, freeing it in turn if it was its
        // last, as vm_object_deallocate's loop does.
        drop(shadow);
    }
}

/// The object all wired kernel memory belongs to, at the offset of its
/// address in the kernel map.
pub static KERNEL_OBJECT: Object = Object::new(MAX_KERNEL_ADDRESS - MIN_KERNEL_ADDRESS + 1);

/// The cache objects come from.
pub(super) static CACHE: Cache<Object> = Cache::new("vm_object");

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
#[expect(
    clippy::too_many_lines,
    reason = "vm_object_terminate's steps, in GNU Mach's order"
)]
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
    // Its pages are cleaned or freed now: a temporary object's, or one with
    // no pager, all; another's dirty or precious ones, given back first,
    // and the rest.
    let mut flush = [0; 32];
    let mut flushing = 0;
    if state.flags.contains(Flags::TEMPORARY) || state.pager.is_none() {
        let size = state.size;
        state.remove_pages(0..size);
    } else {
        let state = &mut *state;
        let mut cursor = state.pages.tree.front_mut();
        while let Some(page) = cursor.get() {
            if page.is_dirty() || page.is_precious() {
                assert!(
                    flushing != flush.len(),
                    "vm_object_terminate: more than 32 pages, not yet"
                );
                flush[flushing] = page.offset();
                flushing += 1;
                cursor.move_next();
            } else {
                let page = cursor.remove().unwrap();
                state.pages.count -= 1;
                page.remove_from_queue();
                super::super::page::alloc::free(page);
            }
        }
    }
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
    // The pages given back first, as vm_pageout_page flushes them.
    if let Some(request) = &request {
        for &offset in &flush[..flushing] {
            // SAFETY: the caller's contract, and the object is not freed
            // yet.
            let state = unsafe { object.as_ref() }.lock();
            let Some(page) = state.lookup(offset) else {
                continue;
            };
            let (copy, count) = crate::vm::pager::page_copy(page);
            drop(state);
            crate::vm::pager::data_return(
                Port::copy_send(&pager),
                Port::make_send(request),
                offset,
                copy,
                count,
                true,
                true,
            );
        }
    }
    match kind {
        Some(Kind::Device(record)) => {
            // memory_object_release: memory_object_terminate, served by the
            // kernel's own pager, called directly; nothing waits for the
            // pager's port.
            // SAFETY: the object served by the record is terminating.
            unsafe {
                pager::terminate(
                    record,
                    &pager,
                    request.expect("device_pager_terminate: no request port"),
                    name.expect("device_pager_terminate: no name port"),
                );
            }
            // The request's destination right, as ipc_kobject_server
            // releases it.
            Port::release_send(pager);
        }
        None => {
            // A user pager's: memory_object_release, with the kernel's send
            // right for its port and the receive rights of the request and
            // name ports.
            crate::vm::pager::memory_object_terminate(
                pager,
                request.expect("memory_object_terminate: no request port"),
                name.expect("memory_object_terminate: no name port"),
            );
        }
    }
    // SAFETY: the caller's contract.
    drop(unsafe { CacheBox::from_raw(&CACHE, object) });
}
