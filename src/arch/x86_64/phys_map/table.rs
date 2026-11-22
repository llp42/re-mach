// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ops::Range;
use core::ptr;
use core::slice;
use core::sync::atomic::{AtomicU64, Ordering};

use x86_64::VirtAddr;
use x86_64::structures::paging::page_table::PageTableEntry;
use x86_64::structures::paging::{self, PageTableFlags, PageTableIndex, Size4KiB};

use super::flags::{
    DIRECT_MAP_PAGE, DIRECT_MAP_SLOT, DIRECTORY_SIZE, EARLY_PAGE_TABLES, ENTRIES, KERNEL_SLOT,
    LARGE_PAGE, LARGE_PAGE_SIZE, PAGE_TABLE_SLOT, SLOT_SIZE, TABLE, TEXT_PAGE, WINDOW_PAGE,
};
use super::phys_map::NO_EXECUTE;
use crate::arch::x86_64::address::PhysAddr;
use crate::arch::x86_64::boot_memory;
use crate::arch::x86_64::memory_layout::{KERNEL_BASE, MIN_KERNEL_ADDRESS, PAGE_SIZE};

/// Index of `address`'s entry in a table whose entries map `size` bytes.
pub(super) const fn index(address: usize, size: usize) -> usize {
    address / size % ENTRIES
}

/// Where the page table window shows the entry that maps `address` at
/// `level`: 0 for its page table's entry, up to 3 for its PML4's.
pub(super) fn window_entry(address: usize, level: usize) -> &'static AtomicU64 {
    let address = VirtAddr::new_truncate(u64::try_from(address).unwrap());
    let indices = [
        address.p4_index(),
        address.p3_index(),
        address.p2_index(),
        address.p1_index(),
    ];
    // The window's slot `level + 1` times, then the address's indices down
    // to the table holding the entry.
    let mut table = [PageTableIndex::new(u16::try_from(PAGE_TABLE_SLOT).unwrap()); 4];
    table[level + 1..].copy_from_slice(&indices[..3 - level]);
    let page =
        paging::Page::<Size4KiB>::from_page_table_indices(table[0], table[1], table[2], table[3]);
    let entry = page.start_address() + u64::from(indices[3 - level]) * 8;
    // SAFETY: the window shows every table of the loaded PML4, as entries.
    unsafe { &*ptr::with_exposed_provenance::<AtomicU64>(usize::try_from(entry.as_u64()).unwrap()) }
}

/// Maps kernel `address` to `physical` with `flags`, no-execute, in the
/// kernel physical map: as every physical map shares kernel space's tables,
/// the entry is written with one atomic operation and no lock.
///
/// # Panics
///
/// If no page table maps `address`, or a page is mapped there already.
pub(super) fn enter_kernel_page(address: usize, physical: PhysAddr, flags: PageTableFlags) {
    // Kernel space's PML4 entries are always there; its directory and page
    // table may not be.
    for level in [2, 1] {
        let entry = PageTableFlags::from_bits_truncate(
            window_entry(address, level).load(Ordering::Relaxed),
        );
        assert!(
            entry.contains(PageTableFlags::PRESENT) && !entry.contains(PageTableFlags::HUGE_PAGE),
            "pmap: no page table at {address:#x}"
        );
    }
    let flags = flags | *NO_EXECUTE.get().expect("pmap: not bootstrapped");
    // A page that was not present is in no TLB, so nothing to invalidate.
    assert!(
        window_entry(address, 0)
            .compare_exchange(
                0,
                frame(physical).as_u64() | flags.bits(),
                Ordering::Release,
                Ordering::Relaxed
            )
            .is_ok(),
        "pmap: {address:#x} mapped already"
    );
}

/// Entry `index` of the table at `table`, through the direct map.
pub(super) fn table_entry(table: PhysAddr, index: usize) -> &'static AtomicU64 {
    // SAFETY: a table is an array of entries, and the direct map reaches all
    // physical memory.
    unsafe { &*table.direct_map_ptr::<AtomicU64>().add(index) }
}

/// The table or page `entry` points at.
pub(super) fn entry_address(entry: u64) -> PhysAddr {
    let address = x86_64::PhysAddr::new_truncate(entry).as_u64();
    PhysAddr(usize::try_from(address).unwrap()).align_down()
}

/// `address` as the `x86_64` crate's.
pub(super) fn frame(address: PhysAddr) -> x86_64::PhysAddr {
    x86_64::PhysAddr::new(u64::try_from(address.0).unwrap())
}

/// `count` zeroed tables from boot memory, contiguous.
pub(super) fn alloc_tables(count: usize) -> PhysAddr {
    let start = boot_memory::alloc_pages(count);
    // SAFETY: fresh boot memory, mapped at the kernel window.
    unsafe {
        start
            .kernel_window_ptr::<u8>()
            .write_bytes(0, count * PAGE_SIZE);
    }
    start
}

/// The entries of the `count` contiguous tables at `start`.
///
/// # Safety
///
/// The tables come from `alloc_tables`, and nothing else refers to them.
pub(super) const unsafe fn entries<'a>(start: PhysAddr, count: usize) -> &'a mut [PageTableEntry] {
    // SAFETY: guaranteed by the caller; an entry is a `u64`, and tables are
    // arrays of entries.
    unsafe { slice::from_raw_parts_mut(start.kernel_window_ptr(), count * ENTRIES) }
}

/// Maps kernel space into `pml4`: all its PDP tables, so its PML4 entries
/// never change, page tables at its start, and the kernel window, with
/// physical memory below `boot_end` in large pages, but where the `image`
/// lies in pages, read-only over `text` and `rodata`; all but the text's
/// pages get `no_execute`.
///
/// # Safety
///
/// Runs on the boot CPU only, after boot memory's bootstrap; `pml4` comes
/// from `alloc_tables`.
pub(super) unsafe fn map_kernel_space(
    pml4: PhysAddr,
    image: Range<PhysAddr>,
    text: Range<PhysAddr>,
    rodata: Range<PhysAddr>,
    boot_end: PhysAddr,
    no_execute: PageTableFlags,
) {
    let slots = ENTRIES - KERNEL_SLOT;
    let pdps = alloc_tables(slots);
    // SAFETY: guaranteed by the caller.
    for (slot, entry) in unsafe { entries(pml4, 1) }
        .iter_mut()
        .skip(KERNEL_SLOT)
        .enumerate()
    {
        entry.set_addr(frame(pdps + slot * PAGE_SIZE), TABLE);
    }
    let pdp = |address| pdps + (index(address, SLOT_SIZE) - KERNEL_SLOT) * PAGE_SIZE;
    let (directory, tables) = (alloc_tables(1), alloc_tables(EARLY_PAGE_TABLES));
    // SAFETY: guaranteed by the caller; fresh tables.
    unsafe {
        entries(pdp(MIN_KERNEL_ADDRESS), 1)[index(MIN_KERNEL_ADDRESS, DIRECTORY_SIZE)]
            .set_addr(frame(directory), TABLE);
        for (table, entry) in entries(directory, 1)
            .iter_mut()
            .take(EARLY_PAGE_TABLES)
            .enumerate()
        {
            entry.set_addr(frame(tables + table * PAGE_SIZE), TABLE);
        }
    }
    let window_size = KERNEL_BASE.wrapping_neg();
    let count = window_size / DIRECTORY_SIZE;
    let large = boot_end.0 / LARGE_PAGE_SIZE;
    let small = window_size / LARGE_PAGE_SIZE - large;
    // The large pages the image lies in.
    let image_pages = image.start.0 / LARGE_PAGE_SIZE..image.end.0.div_ceil(LARGE_PAGE_SIZE);
    let (directories, tables) = (alloc_tables(count), alloc_tables(small));
    let image_tables = alloc_tables(image_pages.len());
    // SAFETY: guaranteed by the caller; fresh tables.
    unsafe {
        for (directory, entry) in entries(pdp(KERNEL_BASE), 1)
            .iter_mut()
            .skip(index(KERNEL_BASE, DIRECTORY_SIZE))
            .enumerate()
        {
            entry.set_addr(frame(directories + directory * PAGE_SIZE), TABLE);
        }
        let window = entries(directories, count);
        for (page, entry) in window[..large].iter_mut().enumerate() {
            if image_pages.contains(&page) {
                let table = image_tables + (page - image_pages.start) * PAGE_SIZE;
                entry.set_addr(frame(table), TABLE);
            } else {
                entry.set_addr(
                    frame(PhysAddr(page * LARGE_PAGE_SIZE)),
                    LARGE_PAGE | no_execute,
                );
            }
        }
        for (table, entry) in window[large..].iter_mut().enumerate() {
            entry.set_addr(frame(tables + table * PAGE_SIZE), TABLE);
        }
        let start = PhysAddr(image_pages.start * LARGE_PAGE_SIZE);
        for (page, entry) in entries(image_tables, image_pages.len())
            .iter_mut()
            .enumerate()
        {
            let address = start + page * PAGE_SIZE;
            let flags = if text.contains(&address) {
                TEXT_PAGE
            } else if rodata.contains(&address) {
                TEXT_PAGE | no_execute
            } else {
                WINDOW_PAGE | no_execute
            };
            entry.set_addr(frame(address), flags);
        }
    }
}

/// Maps the first `size` bytes of physical memory into `pml4`, at the
/// direct map, in large pages with `no_execute`.
///
/// # Safety
///
/// Runs on the boot CPU only, after boot memory's bootstrap; `pml4` comes
/// from `alloc_tables`.
pub(super) unsafe fn map_direct(pml4: PhysAddr, size: usize, no_execute: PageTableFlags) {
    let count = size / DIRECTORY_SIZE;
    let slots = count.div_ceil(ENTRIES);
    let (pdps, directories) = (alloc_tables(slots), alloc_tables(count));
    // SAFETY: guaranteed by the caller; fresh tables.
    unsafe {
        for (page, entry) in entries(directories, count).iter_mut().enumerate() {
            entry.set_addr(
                frame(PhysAddr(page * LARGE_PAGE_SIZE)),
                DIRECT_MAP_PAGE | no_execute,
            );
        }
        for (directory, entry) in entries(pdps, slots).iter_mut().take(count).enumerate() {
            entry.set_addr(frame(directories + directory * PAGE_SIZE), TABLE);
        }
        for (slot, entry) in entries(pml4, 1)
            .iter_mut()
            .skip(DIRECT_MAP_SLOT)
            .take(slots)
            .enumerate()
        {
            entry.set_addr(frame(pdps + slot * PAGE_SIZE), TABLE);
        }
    }
}
