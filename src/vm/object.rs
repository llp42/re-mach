// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Memory objects' kernel side.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::arch::{MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS};
use crate::ipc::port::{self, KernelObject, Port};
use crate::kernel::slab::{Cache, CacheBox};

/// The kernel's side of a memory object; so far only its first values, its
/// name port and its place in the list of persisting objects.
#[expect(
    dead_code,
    reason = "vm_object_reference, vm_object_deallocate and the pager calls read them"
)]
pub struct Object {
    /// Its size, in bytes.
    size: usize,
    /// How many references it has.
    references: u32,
    /// Whether the kernel created it, so the default memory manager manages it.
    internal: bool,
    /// Whether changes stay in memory rather than going back to its manager.
    temporary: bool,
    /// Whether it is not yet terminated.
    alive: bool,
    /// Its name port, which names it to `vm_region`; null until it has one.
    pager_name: AtomicPtr<Port>,
    /// Its place in the list of persisting objects.
    persisting_link: LinkedListAtomicLink,
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
            size,
            references: 1,
            internal: true,
            temporary: true,
            alive: true,
            pager_name: AtomicPtr::new(ptr::null_mut()),
            persisting_link: LinkedListAtomicLink::new(),
        }
    }
}

/// The object all wired kernel memory belongs to.
static KERNEL_OBJECT: Object = Object::new(MAX_KERNEL_ADDRESS - MIN_KERNEL_ADDRESS + 1);

/// The unreferenced objects kept in case they are used again, until memory
/// runs low.
#[expect(dead_code, reason = "vm_object_deallocate and vm_object_lookup use it")]
static PERSISTING: LinkedList<PersistingAdapter> = LinkedList::new(PersistingAdapter::NEW);

/// The cache objects come from.
static CACHE: Cache<Object> = Cache::new("vm_object");

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
    let mut port = port::allocate_kernel()
        .unwrap_or_else(|error| panic!("vm_object_init: ipc_port_alloc_kernel failed: {error:?}"));
    port.set_kernel_object(KernelObject::PagerName(NonNull::from(&KERNEL_OBJECT)));
    KERNEL_OBJECT
        .pager_name
        .store(CacheBox::into_raw(port).as_ptr(), Ordering::Release);
}
