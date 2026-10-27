// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Physical memory at boot, like dfly's `getmemsize` and GNU Mach's
//! `biosmem`: the free ranges of the UEFI memory map, and a page allocator
//! over the largest one.

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::PAGE_SIZE;
use super::serial::Serial;
use crate::uefi::{self, MemoryMap};

/// Most ranges kept of each kind, as GNU Mach's `BIOSMEM_MAX_MAP_SIZE`;
/// more are ignored.
const MAX_RANGES: usize = 128;
/// Free memory starts here, past page 0 and the real-mode data (GNU Mach's
/// `BIOSMEM_BASE`).
const FREE_START: u64 = 0x1_0000;
/// The heap starts here or above, leaving low memory for the APs' startup
/// code (GNU Mach's `BIOSMEM_END`).
const HEAP_START: u64 = 0x10_0000;

/// A physical range, `[start, end)`.
struct Range {
    start: AtomicU64,
    end: AtomicU64,
}

/// What `init` found; atomic, so it's set through `&self`.
struct BootMemory {
    /// Free ranges, sorted and merged.
    free: [Range; MAX_RANGES],
    free_count: AtomicUsize,
    /// Boot-services ranges: free once nothing uses the firmware's tables.
    boot_services: [Range; MAX_RANGES],
    boot_services_count: AtomicUsize,
    /// End of the highest range of either kind.
    end: AtomicU64,
    /// Lowest heap address.
    heap_start: AtomicU64,
    /// Allocations come off the top, as biosmem's.
    heap_top: AtomicU64,
}

static BOOT_MEMORY: BootMemory = BootMemory {
    free: [const {
        Range {
            start: AtomicU64::new(0),
            end: AtomicU64::new(0),
        }
    }; MAX_RANGES],
    free_count: AtomicUsize::new(0),
    boot_services: [const {
        Range {
            start: AtomicU64::new(0),
            end: AtomicU64::new(0),
        }
    }; MAX_RANGES],
    boot_services_count: AtomicUsize::new(0),
    end: AtomicU64::new(0),
    heap_start: AtomicU64::new(0),
    heap_top: AtomicU64::new(0),
};

/// Keeps the map's conventional and boot-services memory from `FREE_START`
/// on, makes the largest free part from `HEAP_START` on the heap, and prints
/// a summary, as dfly's `avail memory` line.
pub fn init(memory_map: MemoryMap) {
    let mut free = [(0, 0); MAX_RANGES];
    let mut free_count = 0;
    let mut boot_services_count = 0;
    let mut end = 0;
    for area in memory_map {
        let start = area.physical_start.max(FREE_START);
        let area_end = area.physical_start + area.number_of_pages * uefi::PAGE_SIZE;
        if start >= area_end {
            continue;
        }
        if area.ty == uefi::CONVENTIONAL_MEMORY && free_count < MAX_RANGES {
            free[free_count] = (start, area_end);
            free_count += 1;
        } else if (area.ty == uefi::BOOT_SERVICES_CODE || area.ty == uefi::BOOT_SERVICES_DATA)
            && boot_services_count < MAX_RANGES
        {
            let range = &BOOT_MEMORY.boot_services[boot_services_count];
            range.start.store(start, Ordering::Relaxed);
            range.end.store(area_end, Ordering::Relaxed);
            boot_services_count += 1;
        } else {
            continue;
        }
        end = end.max(area_end);
    }
    BOOT_MEMORY
        .boot_services_count
        .store(boot_services_count, Ordering::Relaxed);
    BOOT_MEMORY.end.store(end, Ordering::Relaxed);
    store_free(&mut free[..free_count]);

    let (mut heap_start, mut heap_end) = (0, 0);
    for range in free_ranges() {
        let low = range.0.max(HEAP_START);
        if low < range.1 && range.1 - low > heap_end - heap_start {
            (heap_start, heap_end) = (low, range.1);
        }
    }
    BOOT_MEMORY.heap_start.store(heap_start, Ordering::Relaxed);
    BOOT_MEMORY.heap_top.store(heap_end, Ordering::Relaxed);
    print_summary();
}

/// Adds the boot-services ranges to the free ones, once nothing uses the
/// firmware's page tables, descriptor tables or stack, and prints a new
/// summary.
pub fn release_boot_services() {
    let mut found = [(0, 0); 2 * MAX_RANGES];
    let boot_services = BOOT_MEMORY
        .boot_services
        .iter()
        .take(BOOT_MEMORY.boot_services_count.load(Ordering::Relaxed))
        .map(|range| {
            (
                range.start.load(Ordering::Relaxed),
                range.end.load(Ordering::Relaxed),
            )
        });
    let mut count = 0;
    for (slot, range) in found.iter_mut().zip(free_ranges().chain(boot_services)) {
        *slot = range;
        count += 1;
    }
    BOOT_MEMORY.boot_services_count.store(0, Ordering::Relaxed);
    store_free(&mut found[..count]);
    print_summary();
}

/// End of the highest free or boot-services range.
pub fn end() -> u64 {
    BOOT_MEMORY.end.load(Ordering::Relaxed)
}

/// Takes `count` pages off the top of the heap, as biosmem's
/// `biosmem_bootalloc`; `None` if the heap can't hold them.
pub fn allocate_pages(count: usize) -> Option<u64> {
    let size = u64::try_from(count.checked_mul(PAGE_SIZE)?).ok()?;
    let start = BOOT_MEMORY.heap_start.load(Ordering::Relaxed);
    let top = BOOT_MEMORY
        .heap_top
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |top| {
            top.checked_sub(size).filter(|&address| address >= start)
        })
        .ok()?;
    Some(top - size)
}

/// Sorts and merges `found` into the free ranges.
fn store_free(found: &mut [(u64, u64)]) {
    found.sort_unstable();
    let mut count = 0;
    let touching = |a: &(u64, u64), b: &(u64, u64)| b.0 <= a.1;
    for (range, run) in BOOT_MEMORY.free.iter().zip(found.chunk_by(touching)) {
        let start = run[0].0;
        let end = run.iter().map(|&(_, end)| end).max().unwrap_or(start);
        range.start.store(start, Ordering::Relaxed);
        range.end.store(end, Ordering::Relaxed);
        count += 1;
    }
    BOOT_MEMORY.free_count.store(count, Ordering::Relaxed);
}

fn free_ranges() -> impl Iterator<Item = (u64, u64)> {
    BOOT_MEMORY
        .free
        .iter()
        .take(BOOT_MEMORY.free_count.load(Ordering::Relaxed))
        .map(|range| {
            (
                range.start.load(Ordering::Relaxed),
                range.end.load(Ordering::Relaxed),
            )
        })
}

/// One line, as dfly's `avail memory`: the free total, the range count and
/// what's left of the heap.
fn print_summary() {
    let (count, total) = free_ranges().fold((0, 0), |(count, total), (start, end)| {
        (count + 1, total + end - start)
    });
    let heap_start = BOOT_MEMORY.heap_start.load(Ordering::Relaxed);
    let heap_top = BOOT_MEMORY.heap_top.load(Ordering::Relaxed);
    let _ = write!(
        Serial,
        "avail memory = {total} ({} MiB) in {count} ranges, heap {heap_start:#x}-{heap_top:#x}\r\n",
        total >> 20
    );
}
