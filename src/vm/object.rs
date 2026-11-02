// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Memory objects' kernel side.

use crate::arch::{MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS};

/// The kernel's side of a memory object; so far only what the kernel object
/// sets.
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
}

impl Object {
    /// A new object of `size` bytes, with one reference.
    const fn new(size: usize) -> Self {
        Self {
            size,
            references: 1,
            internal: true,
            temporary: true,
            alive: true,
        }
    }
}

/// The object all wired kernel memory belongs to.
#[expect(dead_code, reason = "kmem_alloc and the kernel's pages use it")]
static KERNEL_OBJECT: Object = Object::new(MAX_KERNEL_ADDRESS - MIN_KERNEL_ADDRESS + 1);
