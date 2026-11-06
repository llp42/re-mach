// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Physical maps.

use core::arch::asm;
use core::ops::Range;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use core::{ptr, slice};

use x86_64::instructions::tlb;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr3, Cr3Flags, Cr4, Cr4Flags};
use x86_64::registers::model_specific::{Pat, PatMemoryType};
use x86_64::structures::paging::page_table::PageTableEntry;
use x86_64::structures::paging::{PageTableFlags, PhysFrame};

use super::address::PhysAddr;
use super::boot_memory;
use super::cpuid;
use super::memory_layout::{
    DIRECT_MAP_BASE, KERNEL_BASE, MIN_KERNEL_ADDRESS, PAGE_SIZE, PAGE_TABLE_BASE,
};
use crate::kernel::Error;
use crate::kernel::slab::{Cache, CacheBox};
use crate::sync::OnceLock;
use crate::vm::page;
use crate::vm::pmap::MachinePmap;

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

/// Global 2 MiB page, for mappings never removed.
const LARGE_PAGE: PageTableFlags = TABLE
    .union(PageTableFlags::HUGE_PAGE)
    .union(PageTableFlags::GLOBAL);

/// Direct map page, accessed and dirty preset so a split keeps them.
const DIRECT_MAP_PAGE: PageTableFlags = LARGE_PAGE
    .union(PageTableFlags::ACCESSED)
    .union(PageTableFlags::DIRTY);

/// Kernel memory's page: writable.
const KERNEL_PAGE: PageTableFlags = TABLE;

/// Device memory's page: writable, uncached (PAT entry 3).
const DEVICE_PAGE: PageTableFlags = TABLE
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

/// One address space's page tables.
#[expect(dead_code, reason = "the VM reads it")]
pub struct Pmap {
    /// Its PML4 table.
    pml4: PhysAddr,
    /// References held on it.
    references: AtomicUsize,
    /// The CPUs it is active on, a bit each.
    active_cpus: AtomicUsize,
}

impl MachinePmap for Pmap {
    fn steal_memory(size: usize) -> *mut u8 {
        boot_memory::steal_pages(size.div_ceil(PAGE_SIZE)).direct_map_ptr()
    }

    fn kernel() -> &'static Self {
        KERNEL_PMAP.get().expect("pmap: not bootstrapped")
    }

    fn create() -> Result<&'static Self, Error> {
        let mut pmap = CACHE.allocate(Self {
            pml4: PhysAddr(0),
            references: AtomicUsize::new(1),
            active_cpus: AtomicUsize::new(0),
        })?;
        // In no object, so at index 0.
        let pml4 = page::allocate_zeroed(0).expect("pmap_create").address();
        // SAFETY: the new table is zeroed and the pmap's alone; the kernel's
        // is read atomically, as the CPU sets its accessed bits. Tables are
        // arrays of entries, reached through the direct map.
        let (entries, kernel_entries) = unsafe {
            (
                slice::from_raw_parts_mut(pml4.direct_map_ptr::<u64>(), ENTRIES),
                slice::from_raw_parts(Self::kernel().pml4.direct_map_ptr::<AtomicU64>(), ENTRIES),
            )
        };
        // The kernel's entries past the window's are set once at boot.
        for (entry, kernel_entry) in entries[PAGE_TABLE_SLOT + 1..]
            .iter_mut()
            .zip(&kernel_entries[PAGE_TABLE_SLOT + 1..])
        {
            *entry = kernel_entry.load(Ordering::Relaxed);
        }
        entries[PAGE_TABLE_SLOT] = frame(pml4).as_u64() | TABLE.bits();
        pmap.pml4 = pml4;
        // SAFETY: a pmap lives until pmap_destroy frees it, which comes later.
        Ok(unsafe { CacheBox::into_raw(pmap).as_ref() })
    }

    fn reference(&self) {
        self.references.fetch_add(1, Ordering::Relaxed);
    }

    fn enter_kernel(address: usize, physical: PhysAddr) {
        enter_kernel_page(address, physical, KERNEL_PAGE);
    }

    fn extract_kernel(address: usize) -> Option<PhysAddr> {
        // Kernel space's PML4 entries are always there.
        for level in [2, 1] {
            let entry = PageTableFlags::from_bits_truncate(
                window_entry(address, level).load(Ordering::Relaxed),
            );
            if !entry.contains(PageTableFlags::PRESENT) || entry.contains(PageTableFlags::HUGE_PAGE)
            {
                return None;
            }
        }
        let entry = window_entry(address, 0).load(Ordering::Relaxed);
        PageTableFlags::from_bits_truncate(entry)
            .contains(PageTableFlags::PRESENT)
            .then(|| {
                let frame = x86_64::PhysAddr::new_truncate(entry).as_u64();
                PhysAddr(usize::try_from(frame).unwrap()).align_down()
            })
    }

    fn enter_device(address: Range<usize>, physical: PhysAddr) {
        for offset in (0..address.end - address.start).step_by(PAGE_SIZE) {
            enter_kernel_page(address.start + offset, physical + offset, DEVICE_PAGE);
        }
    }

    fn boot_space() -> Range<usize> {
        BOOT_SPACE.get().expect("pmap: not bootstrapped").clone()
    }

    fn init() {
        CACHE.register();
    }
}

/// The cache pmaps come from.
static CACHE: Cache<Pmap> = Cache::new("pmap");

/// The kernel's physical map, once `bootstrap` builds it.
static KERNEL_PMAP: OnceLock<Pmap> = OnceLock::new();

/// The kernel addresses the boot mappings use, once `bootstrap` maps them.
static BOOT_SPACE: OnceLock<Range<usize>> = OnceLock::new();

/// End of the direct map, once `bootstrap` sets it.
static DIRECT_MAP_END: OnceLock<usize> = OnceLock::new();

/// Index of `address`'s entry in a table whose entries map `size` bytes.
const fn index(address: usize, size: usize) -> usize {
    address / size % ENTRIES
}

/// Where the page table window shows the entry that maps `address` at
/// `level`: 0 for its page table's entry, up to 3 for its PML4's.
fn window_entry(address: usize, level: usize) -> &'static AtomicU64 {
    let base = (0..level).fold(PAGE_TABLE_BASE, |base, step| {
        base + (PAGE_TABLE_SLOT << (30 - 9 * step))
    });
    let index = (address >> (12 + 9 * level)) & ((1 << (36 - 9 * level)) - 1);
    // SAFETY: the window shows every table of the loaded PML4, as entries.
    unsafe { &*ptr::with_exposed_provenance::<AtomicU64>(base + index * 8) }
}

/// Maps kernel `address` to `physical` with `flags`, in the kernel pmap: as
/// every pmap shares kernel space's tables, the entry is written with one
/// atomic operation and no lock.
///
/// # Panics
///
/// If no page table maps `address`, or a page is mapped there already.
fn enter_kernel_page(address: usize, physical: PhysAddr, flags: PageTableFlags) {
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

/// `address` as the `x86_64` crate's.
fn frame(address: PhysAddr) -> x86_64::PhysAddr {
    x86_64::PhysAddr::new(u64::try_from(address.0).unwrap())
}

/// `count` zeroed tables from boot memory, contiguous.
fn alloc_tables(count: usize) -> PhysAddr {
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
const unsafe fn entries<'a>(start: PhysAddr, count: usize) -> &'a mut [PageTableEntry] {
    // SAFETY: guaranteed by the caller; an entry is a `u64`, and tables are
    // arrays of entries.
    unsafe { slice::from_raw_parts_mut(start.kernel_window_ptr(), count * ENTRIES) }
}

/// Builds the kernel's page tables and physical map, switches to them, and
/// sets the page attribute table.
///
/// # Panics
///
/// If boot memory runs out, physical memory goes past the direct map, or
/// on a second call.
///
/// # Safety
///
/// Runs once, on the boot CPU, after boot memory's bootstrap.
pub unsafe fn bootstrap() {
    let image_end = PhysAddr((&raw const super::_end).addr() - KERNEL_BASE);
    let (heap_end, memory_end) = (boot_memory::heap_end(), boot_memory::memory_end());
    let boot_end = image_end.max(heap_end).0.next_multiple_of(LARGE_PAGE_SIZE);
    let direct_map_size = memory_end
        .0
        .next_multiple_of(DIRECTORY_SIZE)
        .max(MIN_DIRECT_MAP_SIZE);
    assert!(
        direct_map_size <= (KERNEL_SLOT - DIRECT_MAP_SLOT) * SLOT_SIZE,
        "pmap: physical memory past the direct map"
    );
    let pml4 = alloc_tables(1);
    // SAFETY: guaranteed by the caller; the PML4 was just allocated.
    unsafe {
        map_kernel_space(pml4, PhysAddr(boot_end));
        map_direct(pml4, direct_map_size);
        entries(pml4, 1)[PAGE_TABLE_SLOT].set_addr(frame(pml4), TABLE);
    }
    let kernel_pmap = Pmap {
        pml4,
        references: AtomicUsize::new(1),
        active_cpus: AtomicUsize::new(usize::MAX),
    };
    assert!(
        KERNEL_PMAP.set(kernel_pmap).is_ok()
            && BOOT_SPACE.set(KERNEL_BASE..KERNEL_BASE + boot_end).is_ok()
            && DIRECT_MAP_END
                .set(DIRECT_MAP_BASE + direct_map_size)
                .is_ok(),
        "pmap: bootstrapped twice"
    );
    let pml4_frame = PhysFrame::containing_address(frame(pml4));
    // SAFETY: the new tables map the kernel window up to `boot_end`, which
    // holds all the kernel reaches; nothing uses the identity map any more.
    unsafe {
        Cr4::update(|flags| flags.insert(Cr4Flags::PAGE_GLOBAL));
        Cr3::write(pml4_frame, Cr3Flags::empty());
    }
    // SAFETY: guaranteed by the caller; no mapping uses PAT entries 4 to 7.
    unsafe { set_page_attributes() };
}

/// Loads `PAGE_ATTRIBUTES` if the CPU has a PAT, with global pages off and
/// caches disabled and flushed around the write.
///
/// # Safety
///
/// Runs on the boot CPU only, while no mapping uses entries 4 to 7.
unsafe fn set_page_attributes() {
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

/// Writes back and invalidates the caches, then flushes the TLB.
fn flush_caches() {
    // SAFETY: only writes back and drops cached lines.
    unsafe { asm!("wbinvd", options(att_syntax, nostack, preserves_flags)) };
    tlb::flush_all();
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
    let (directories, tables) = (alloc_tables(count), alloc_tables(small));
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
    let (pdps, directories) = (alloc_tables(slots), alloc_tables(count));
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
