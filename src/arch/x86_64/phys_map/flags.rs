// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};
use x86_64::registers::model_specific::{Pat, PatMemoryType};
use x86_64::structures::paging::PageTableFlags;

use super::flush::flush_caches;
use super::table::index;
use crate::arch::x86_64::cpuid;
use crate::arch::x86_64::memory_layout::{DIRECT_MAP_BASE, MIN_KERNEL_ADDRESS, PAGE_TABLE_BASE};

/// Entries per table.
pub(super) const ENTRIES: usize = 512;

/// Bytes a page directory entry maps as one large page.
pub(super) const LARGE_PAGE_SIZE: usize = 2 << 20;

/// Bytes one page directory maps.
pub(super) const DIRECTORY_SIZE: usize = 1 << 30;

/// Bytes one PML4 slot maps.
pub(super) const SLOT_SIZE: usize = 1 << 39;

/// Bytes one entry maps at each level above the page tables, from the PML4
/// down.
pub(super) const UPPER_LEVELS: [usize; 3] = [SLOT_SIZE, DIRECTORY_SIZE, LARGE_PAGE_SIZE];

/// PML4 slots of the page table window, the direct map and kernel space.
pub(super) const PAGE_TABLE_SLOT: usize = index(PAGE_TABLE_BASE, SLOT_SIZE);
pub(super) const DIRECT_MAP_SLOT: usize = index(DIRECT_MAP_BASE, SLOT_SIZE);
pub(super) const KERNEL_SLOT: usize = index(MIN_KERNEL_ADDRESS, SLOT_SIZE);

/// Least memory the direct map covers: all of the first 4 GiB.
pub(super) const MIN_DIRECT_MAP_SIZE: usize = 4 << 30;

/// Page tables ready at the start of kernel space, for the VM's first
/// allocations.
pub(super) const EARLY_PAGE_TABLES: usize = 128;

/// Entry of a lower table.
pub(super) const TABLE: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::WRITABLE);

/// Global 2 MiB page, for mappings never removed.
pub(super) const LARGE_PAGE: PageTableFlags = TABLE
    .union(PageTableFlags::HUGE_PAGE)
    .union(PageTableFlags::GLOBAL);

/// Global page of the kernel window, for mappings never removed.
pub(super) const WINDOW_PAGE: PageTableFlags = TABLE.union(PageTableFlags::GLOBAL);

/// Read-only kernel page: text, or rodata with `NO_EXECUTE`.
pub(super) const TEXT_PAGE: PageTableFlags = WINDOW_PAGE.difference(PageTableFlags::WRITABLE);

/// Direct map page, accessed and dirty preset so a split keeps them.
pub(super) const DIRECT_MAP_PAGE: PageTableFlags = LARGE_PAGE
    .union(PageTableFlags::ACCESSED)
    .union(PageTableFlags::DIRTY);

/// Kernel memory's page: writable.
pub(super) const KERNEL_PAGE: PageTableFlags = TABLE;

/// A wired mapping's software bit.
pub(super) const WIRED: PageTableFlags = PageTableFlags::BIT_9;

/// Entry of a lower table in a user physical map.
pub(super) const USER_TABLE: PageTableFlags = TABLE.union(PageTableFlags::USER_ACCESSIBLE);

/// User memory's page, accessed preset: read-only and executable unless
/// `enter` adds writing or `NO_EXECUTE`.
pub(super) const USER_PAGE: PageTableFlags = PageTableFlags::PRESENT
    .union(PageTableFlags::USER_ACCESSIBLE)
    .union(PageTableFlags::ACCESSED);

/// Device memory's page: writable, uncached (PAT entry 3).
pub(super) const DEVICE_PAGE: PageTableFlags = TABLE
    .union(PageTableFlags::WRITE_THROUGH)
    .union(PageTableFlags::NO_CACHE);

/// The page attribute table: entries 0 to 3 keep their reset types, so a
/// PTE's PWT and PCD bits keep their meaning; 5 and 6 add write-protect and
/// write-combining.
const PAGE_ATTRIBUTES: [PatMemoryType; 8] = [
    PatMemoryType::WriteBack,
    PatMemoryType::WriteThrough,
    PatMemoryType::Uncacheable,
    PatMemoryType::StrongUncacheable,
    PatMemoryType::WriteBack,
    PatMemoryType::WriteProtected,
    PatMemoryType::WriteCombining,
    PatMemoryType::StrongUncacheable,
];

/// Loads `PAGE_ATTRIBUTES` if the CPU has a PAT, with global pages off and
/// caches disabled and flushed around the write.
///
/// # Safety
///
/// Runs once on each CPU, while no mapping uses entries 4 to 7.
pub(in crate::arch::x86_64) unsafe fn set_page_attributes() {
    if !cpuid::features().has_pat() {
        return;
    }
    let (cr0, cr4) = (Cr0::read(), Cr4::read());
    // SAFETY: guaranteed by the caller; caches and global pages come back
    // as they were.
    unsafe {
        Cr4::write(cr4 - Cr4Flags::PAGE_GLOBAL);
        Cr0::write((cr0 | Cr0Flags::CACHE_DISABLE) - Cr0Flags::NOT_WRITE_THROUGH);
        flush_caches();
        Pat::write(PAGE_ATTRIBUTES);
        flush_caches();
        Cr0::write(cr0);
        Cr4::write(cr4);
    }
}
