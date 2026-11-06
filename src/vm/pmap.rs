// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! What the VM asks of the machine's page tables.

use core::ops::Range;

use crate::arch::PhysAddr;

/// The machine's physical maps.
pub trait MachinePmap {
    /// Takes `size` bytes of physical memory for good, before the page
    /// allocator runs; returns where the kernel reaches them.
    fn steal_memory(size: usize) -> *mut u8;

    /// The kernel's physical map.
    fn kernel() -> &'static Self;

    /// Takes a reference on it.
    fn reference(&self);

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
