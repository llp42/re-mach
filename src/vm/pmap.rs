// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! What the VM asks of the machine's page tables.

use core::ops::Range;

use crate::arch::PhysAddr;
use crate::error::Error;

/// The machine's physical maps.
pub trait MachinePmap {
    /// Takes `size` bytes of physical memory for good, before the page
    /// allocator runs; returns where the kernel reaches them.
    fn steal_memory(size: usize) -> *mut u8;

    /// The kernel's physical map.
    fn kernel() -> &'static Self;

    /// A new physical map for a user task, with one reference, sharing
    /// kernel space and mapping nothing else.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if it can't be allocated.
    fn create() -> Result<&'static Self, Error>;

    /// Takes a reference on it.
    fn reference(&self);

    /// Makes it the current CPU's, as the CPU switches to a thread of its
    /// task from one of another, inside a critical section.
    ///
    /// # Panics
    ///
    /// If it is the kernel's: not yet.
    fn activate(&self);
    /// Gives it up on the current CPU, as the CPU switches from a thread of
    /// its task to one of another, inside a critical section.
    ///
    /// # Panics
    ///
    /// If it is a user task's: not yet.
    fn deactivate(&self);

    /// Maps kernel `address` to the page at `physical`, read-write.
    fn enter_kernel(address: usize, physical: PhysAddr);
    /// The page `enter_kernel` mapped kernel `address` to; `None` if none.
    fn extract_kernel(address: usize) -> Option<PhysAddr>;
    /// Maps the kernel addresses of `address` to the device memory from
    /// `physical` on, uncached.
    fn enter_device(address: Range<usize>, physical: PhysAddr);
    /// The kernel addresses the boot mappings use.
    fn boot_space() -> Range<usize>;

    /// Sets up what physical maps need, once the kernel map is up.
    fn init();
}
