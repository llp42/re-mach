// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Physical maps.

use core::cell::UnsafeCell;
use core::ops::Range;
use core::ptr;
use core::slice;
use core::sync::atomic::{AtomicUsize, Ordering};

use x86_64::registers::control::{Cr3, Cr3Flags, Cr4, Cr4Flags};
use x86_64::structures::paging::page_table::PageTableEntry;
use x86_64::structures::paging::{PageTable, PageTableFlags, PhysFrame};

use super::address::PhysAddr;
use super::boot_memory;
use super::memory_layout::{
    DIRECT_MAP_BASE, KERNEL_BASE, MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE,
    PAGE_TABLE_BASE, phys_to_virt,
};

/// Entries per table.
const ENTRIES: usize = 512;

/// Bytes a page directory entry maps as one large page.
const LARGE_PAGE_SIZE: usize = 2 << 20;

/// Bytes one page directory maps.
const DIRECTORY_SIZE: usize = 1 << 30;

/// Bytes one PML4 slot maps.
const SLOT_SIZE: usize = 1 << 39;

/// PML4 slots of the page table window, the direct map and kernel space.
const PAGE_TABLE_SLOT: usize = index(PAGE_TABLE_BASE, SLOT_SIZE);
const DIRECT_MAP_SLOT: usize = index(DIRECT_MAP_BASE, SLOT_SIZE);
const KERNEL_SLOT: usize = index(MIN_KERNEL_ADDRESS, SLOT_SIZE);

/// Least memory the direct map covers: all of the first 4 GiB.
const MIN_DIRECT_MAP_SIZE: usize = 4 << 30;

/// Page tables ready at the start of kernel space, for the VM's first
/// allocations.
const EARLY_PAGE_TABLES: usize = 128;

/// Entry of a lower table.
const TABLE: PageTableFlags = PageTableFlags::PRESENT.union(PageTableFlags::WRITABLE);

/// Global 2 MiB page.
const LARGE_PAGE: PageTableFlags = TABLE
    .union(PageTableFlags::HUGE_PAGE)
    .union(PageTableFlags::GLOBAL);

/// Direct map page, accessed and dirty preset so a split keeps them.
const DIRECT_MAP_PAGE: PageTableFlags = LARGE_PAGE
    .union(PageTableFlags::ACCESSED)
    .union(PageTableFlags::DIRTY);

/// One address space's page tables.
#[expect(dead_code, reason = "the VM reads it")]
pub struct Pmap {
    /// Its PML4 table.
    pml4: *mut PageTable,
    /// References held on it.
    reference_count: AtomicUsize,
    /// The CPUs it is active on, a bit each.
    active_cpus: AtomicUsize,
}

struct KernelPmap(UnsafeCell<Pmap>);

// SAFETY: written once by `bootstrap`, before any reader.
unsafe impl Sync for KernelPmap {}

/// The kernel's physical map; empty until `bootstrap`.
static KERNEL_PMAP: KernelPmap = KernelPmap(UnsafeCell::new(Pmap {
    pml4: ptr::null_mut(),
    reference_count: AtomicUsize::new(0),
    active_cpus: AtomicUsize::new(0),
}));

/// Free kernel virtual space below the kernel window.
#[expect(dead_code, reason = "the VM takes it")]
const VIRTUAL_BELOW_KERNEL: Range<usize> = MIN_KERNEL_ADDRESS..KERNEL_BASE;

struct VirtualArea(UnsafeCell<Range<usize>>);

// SAFETY: written once by `bootstrap`, before any reader.
unsafe impl Sync for VirtualArea {}

/// Free kernel virtual space above the boot mapping; empty until
/// `bootstrap`.
static VIRTUAL_ABOVE_BOOT: VirtualArea = VirtualArea(UnsafeCell::new(0..0));

/// End of the direct map; 0 until `bootstrap`.
static DIRECT_MAP_END: AtomicUsize = AtomicUsize::new(0);

/// Index of `address`'s entry in a table whose entries map `size` bytes.
const fn index(address: usize, size: usize) -> usize {
    address / size % ENTRIES
}

/// `address` as the `x86_64` crate's.
fn frame(address: PhysAddr) -> x86_64::PhysAddr {
    x86_64::PhysAddr::new(u64::try_from(address.0).unwrap())
}

/// `count` zeroed tables from boot memory, contiguous.
///
/// # Safety
///
/// Runs on the boot CPU only, after boot memory's bootstrap.
unsafe fn alloc_tables(count: usize) -> PhysAddr {
    // SAFETY: guaranteed by the caller.
    let start = unsafe { boot_memory::alloc_pages(count) };
    // SAFETY: fresh boot memory, mapped at the kernel window.
    unsafe { phys_to_virt::<u8>(start).write_bytes(0, count * PAGE_SIZE) };
    start
}

/// The entries of the `count` contiguous tables at `start`.
///
/// # Safety
///
/// The tables come from `alloc_tables`, and nothing else refers to them.
const unsafe fn entries<'a>(start: PhysAddr, count: usize) -> &'a mut [PageTableEntry] {
    // SAFETY: guaranteed by the caller; an entry is a `u64`, and tables are
    // arrays of entries.
    unsafe { slice::from_raw_parts_mut(phys_to_virt(start), count * ENTRIES) }
}

/// Builds the kernel's page tables and physical map, and switches to them.
///
/// # Panics
///
/// If boot memory runs out, or physical memory goes past the direct map.
///
/// # Safety
///
/// Runs once, on the boot CPU, after boot memory's bootstrap.
pub unsafe fn bootstrap() {
    let image_end = PhysAddr((&raw const super::_end).addr() - KERNEL_BASE);
    // SAFETY: guaranteed by the caller.
    let (heap_end, memory_end) = unsafe { (boot_memory::heap_end(), boot_memory::memory_end()) };
    let boot_end = image_end.max(heap_end).0.next_multiple_of(LARGE_PAGE_SIZE);
    let direct_map_size = memory_end
        .0
        .next_multiple_of(DIRECTORY_SIZE)
        .max(MIN_DIRECT_MAP_SIZE);
    assert!(
        direct_map_size <= (KERNEL_SLOT - DIRECT_MAP_SLOT) * SLOT_SIZE,
        "pmap: physical memory past the direct map"
    );
    // SAFETY: guaranteed by the caller.
    let pml4 = unsafe { alloc_tables(1) };
    // SAFETY: guaranteed by the caller; the PML4 was just allocated.
    unsafe {
        map_kernel_space(pml4, PhysAddr(boot_end));
        map_direct(pml4, direct_map_size);
        entries(pml4, 1)[PAGE_TABLE_SLOT].set_addr(frame(pml4), TABLE);
    }
    // SAFETY: guaranteed by the caller; no reader yet.
    unsafe {
        *KERNEL_PMAP.0.get() = Pmap {
            pml4: phys_to_virt(pml4),
            reference_count: AtomicUsize::new(1),
            active_cpus: AtomicUsize::new(usize::MAX),
        };
        *VIRTUAL_ABOVE_BOOT.0.get() = KERNEL_BASE + boot_end..MAX_KERNEL_ADDRESS - PAGE_SIZE + 1;
    }
    DIRECT_MAP_END.store(DIRECT_MAP_BASE + direct_map_size, Ordering::Relaxed);
    let pml4_frame = PhysFrame::containing_address(frame(pml4));
    // SAFETY: the new tables map the kernel window up to `boot_end`, which
    // holds all the kernel reaches; nothing uses the identity map any more.
    unsafe {
        Cr4::update(|flags| flags.insert(Cr4Flags::PAGE_GLOBAL));
        Cr3::write(pml4_frame, Cr3Flags::empty());
    }
}

/// Maps kernel space into `pml4`: all its PDP tables, so its PML4 entries
/// never change, page tables at its start, and the kernel window, with
/// physical memory below `boot_end` in large pages.
///
/// # Safety
///
/// Runs on the boot CPU only, after boot memory's bootstrap; `pml4` comes
/// from `alloc_tables`.
unsafe fn map_kernel_space(pml4: PhysAddr, boot_end: PhysAddr) {
    let slots = ENTRIES - KERNEL_SLOT;
    // SAFETY: guaranteed by the caller.
    let pdps = unsafe { alloc_tables(slots) };
    // SAFETY: guaranteed by the caller.
    for (slot, entry) in unsafe { entries(pml4, 1) }
        .iter_mut()
        .skip(KERNEL_SLOT)
        .enumerate()
    {
        entry.set_addr(frame(pdps + slot * PAGE_SIZE), TABLE);
    }
    let pdp = |address| pdps + (index(address, SLOT_SIZE) - KERNEL_SLOT) * PAGE_SIZE;
    // SAFETY: guaranteed by the caller.
    let (directory, tables) = unsafe { (alloc_tables(1), alloc_tables(EARLY_PAGE_TABLES)) };
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
    // SAFETY: guaranteed by the caller.
    let (directories, tables) = unsafe { (alloc_tables(count), alloc_tables(small)) };
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
            entry.set_addr(frame(PhysAddr(page * LARGE_PAGE_SIZE)), LARGE_PAGE);
        }
        for (table, entry) in window[large..].iter_mut().enumerate() {
            entry.set_addr(frame(tables + table * PAGE_SIZE), TABLE);
        }
    }
}

/// Maps the first `size` bytes of physical memory into `pml4`, at the
/// direct map, in large pages.
///
/// # Safety
///
/// Runs on the boot CPU only, after boot memory's bootstrap; `pml4` comes
/// from `alloc_tables`.
unsafe fn map_direct(pml4: PhysAddr, size: usize) {
    let count = size / DIRECTORY_SIZE;
    let slots = count.div_ceil(ENTRIES);
    // SAFETY: guaranteed by the caller.
    let (pdps, directories) = unsafe { (alloc_tables(slots), alloc_tables(count)) };
    // SAFETY: guaranteed by the caller; fresh tables.
    unsafe {
        for (page, entry) in entries(directories, count).iter_mut().enumerate() {
            entry.set_addr(frame(PhysAddr(page * LARGE_PAGE_SIZE)), DIRECT_MAP_PAGE);
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
