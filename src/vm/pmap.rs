// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! What the VM asks of the machine's page tables.

use core::ops::Range;

/// The machine's physical maps.
pub trait MachinePmap {
    /// Takes `size` bytes of physical memory for good, before the page
    /// allocator runs; returns where the kernel reaches them.
    fn steal_memory(size: usize) -> *mut u8;

    /// The kernel's physical map.
    fn kernel() -> &'static Self;

    /// The kernel addresses the boot mappings use.
    fn boot_space() -> Range<usize>;
}
