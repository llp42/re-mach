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

/// Most free ranges kept, as GNU Mach's `BIOSMEM_MAX_MAP_SIZE`; more are ignored.
const MAX_RANGES: usize = 128;
/// Free memory starts here, past page 0 and the real-mode data (GNU Mach's
/// `BIOSMEM_BASE`).
const FREE_START: u64 = 0x1_0000;
/// The heap starts here or above, leaving low memory for the APs' startup
/// code (GNU Mach's `BIOSMEM_END`).
const HEAP_START: u64 = 0x10_0000;

/// A free physical range, `[start, end)`.
struct Range {
    start: AtomicU64,
    end: AtomicU64,
}

/// What `init` found; atomic, so it's set through `&self`.
struct BootMemory {
    /// Sorted and merged.
    ranges: [Range; MAX_RANGES],
    count: AtomicUsize,
    /// Lowest heap address.
    heap_start: AtomicU64,
    /// Allocations come off the top, as biosmem's.
    heap_top: AtomicU64,
}

static BOOT_MEMORY: BootMemory = BootMemory {
    ranges: [const {
        Range {
            start: AtomicU64::new(0),
            end: AtomicU64::new(0),
        }
    }; MAX_RANGES],
    count: AtomicUsize::new(0),
    heap_start: AtomicU64::new(0),
    heap_top: AtomicU64::new(0),
};

/// Keeps the map's conventional memory from `FREE_START` on, sorted and
/// merged, makes the largest part from `HEAP_START` on the heap, and prints a
/// summary, as dfly's `avail memory` line.
pub fn init(memory_map: MemoryMap) {
    let mut found = [(0, 0); MAX_RANGES];
    let mut count = 0;
    for area in memory_map {
        let start = area.physical_start.max(FREE_START);
        let end = area.physical_start + area.number_of_pages * uefi::PAGE_SIZE;
        if area.ty == uefi::CONVENTIONAL_MEMORY && start < end && count < MAX_RANGES {
            found[count] = (start, end);
            count += 1;
        }
    }
    let found = &mut found[..count];
    found.sort_unstable();

    let mut count = 0;
    let mut total = 0;
    let (mut heap_start, mut heap_end) = (0, 0);
    let touching = |a: &(u64, u64), b: &(u64, u64)| b.0 <= a.1;
    for (range, run) in BOOT_MEMORY.ranges.iter().zip(found.chunk_by(touching)) {
        let start = run[0].0;
        let end = run.iter().map(|&(_, end)| end).max().unwrap_or(start);
        range.start.store(start, Ordering::Relaxed);
        range.end.store(end, Ordering::Relaxed);
        count += 1;
        total += end - start;
        let low = start.max(HEAP_START);
        if low < end && end - low > heap_end - heap_start {
            (heap_start, heap_end) = (low, end);
        }
    }
    BOOT_MEMORY.count.store(count, Ordering::Relaxed);
    BOOT_MEMORY.heap_start.store(heap_start, Ordering::Relaxed);
    BOOT_MEMORY.heap_top.store(heap_end, Ordering::Relaxed);
    let _ = write!(
        Serial,
        "avail memory = {total} ({} MiB) in {count} ranges, heap {heap_start:#x}-{heap_end:#x}\r\n",
        total >> 20
    );
}

/// Takes `count` pages off the top of the heap, as biosmem's
/// `biosmem_bootalloc`; `None` if the heap can't hold them.
#[expect(dead_code, reason = "no caller until pmap_bootstrap")]
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
