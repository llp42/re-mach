// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, Ordering};

use intrusive_collections::{LinkedListAtomicLink, RBTree, UnsafeRef, intrusive_adapter};

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
    persisting_link: LinkedListAtomicLink,
    /// Its state, behind its token.
    pub(super) state: Token<ObjectState>,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub(super) PersistingAdapter = UnsafeRef<Object>: Object { persisting_link => LinkedListAtomicLink }
);

impl Object {
    /// A new object of `size` bytes, with one reference.
    pub(super) const fn new(size: usize) -> Self {
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
    pub(in crate::vm) fn references(&self) -> u32 {
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
