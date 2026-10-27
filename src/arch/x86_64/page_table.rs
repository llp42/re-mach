// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Page tables for 4-level paging: four levels of 512 entries.

use core::sync::atomic::{AtomicU64, Ordering};

/// Entries per table.
pub const ENTRIES: usize = 512;

/// Entry bit P: present.
pub const PRESENT: u64 = 1;
/// Entry bit R/W: writable.
pub const WRITABLE: u64 = 1 << 1;
/// Entry bit PS in a page directory: the entry maps a 2 MiB page.
pub const LARGE_PAGE: u64 = 1 << 7;
/// Entry bit XD: no instruction fetches; needs `IA32_EFER.NXE`.
pub const EXECUTE_DISABLE: u64 = 1 << 63;

/// A table at any level; atomic, because the CPU sets accessed and dirty bits.
#[repr(C, align(4096))]
pub struct PageTable([AtomicU64; ENTRIES]);

impl PageTable {
    /// Points entry `index` at `physical`, with `flags`.
    pub fn set(&self, index: usize, physical: u64, flags: u64) {
        self.0[index].store(physical | flags, Ordering::Relaxed);
    }

    /// Makes entry `index` not present.
    pub fn clear(&self, index: usize) {
        self.0[index].store(0, Ordering::Relaxed);
    }
}

/// The entry `address` uses at `level`: 3 in the PML4, 0 in a page table.
pub fn index(address: u64, level: u32) -> usize {
    usize::try_from((address >> (12 + 9 * level)) & 0x1ff).unwrap()
}
