// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Memory objects' kernel side.

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::arch::{MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS};
use crate::kernel::slab::Cache;

/// The kernel's side of a memory object; so far only its first values and
/// its place in the list of persisting objects.
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
            persisting_link: LinkedListAtomicLink::new(),
        }
    }
}

/// The object all wired kernel memory belongs to.
#[expect(dead_code, reason = "kmem_alloc and the kernel's pages use it")]
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
