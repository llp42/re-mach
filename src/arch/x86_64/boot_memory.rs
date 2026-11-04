// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Boot-time physical memory.

use core::fmt::{self, Write};
use core::ops::Range;

use super::address::PhysAddr;
use super::ap_boot;
use super::memory_layout::{
    DIRECT_MAP_LIMIT, DMA_LIMIT, DMA32_LIMIT, HIGH_MEMORY_LIMIT, PAGE_SIZE, Segment,
};
use super::serial::Serial;
use crate::boot::multiboot2::{Info, MemoryType};
use crate::collections::ArrayVec;
use crate::config::MAX_CPUS;
use crate::sync::BootCell;
use crate::vm::page;

/// Start of usable low memory; the first 64 KiB are left to BIOS data and
/// to buggy hardware.
const LOW_MEMORY_BASE: PhysAddr = PhysAddr(0x1_0000);

/// End of the first MiB.
const LOW_MEMORY_END: PhysAddr = PhysAddr(0x10_0000);

const MAX_BOOT_DATA: usize = 64;

/// How long boot data is kept.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Retention {
    /// Released once no longer needed.
    Temporary,
    /// Kept for good.
    Permanent,
}

/// Boot data descriptor.
struct BootData {
    range: Range<PhysAddr>,
    retention: Retention,
}

impl BootData {
    /// Clips `area` to the part before this boot data, or after it when it
    /// covers the start of `area`; `None` if nothing is left.
    ///
    /// # Panics
    ///
    /// If rounding the end of this boot data overflows.
    fn clip(&self, area: Range<PhysAddr>) -> Option<Range<PhysAddr>> {
        let start = self.range.start.align_down();
        let end = self
            .range
            .end
            .checked_align_up()
            .expect("boot_memory: invalid boot data");
        if end <= area.start || start >= area.end {
            Some(area)
        } else if start > area.start {
            Some(area.start..start)
        } else if end >= area.end {
            None
        } else {
            Some(end..area.end)
        }
    }
}

/// Entries kept from the boot loader's map; overlap resolution can double them.
const MAX_MAP_ENTRIES: usize = 128;

/// Memory range kinds; later ones are more restrictive and win overlaps.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EntryKind {
    Available,
    Reserved,
    Acpi,
    Nvs,
    Unusable,
    #[expect(dead_code, reason = "no boot source reports it")]
    Disabled,
}

impl From<MemoryType> for EntryKind {
    fn from(ty: MemoryType) -> Self {
        match ty {
            MemoryType::AVAILABLE => Self::Available,
            MemoryType::ACPI_RECLAIMABLE => Self::Acpi,
            MemoryType::NVS => Self::Nvs,
            MemoryType::BADRAM => Self::Unusable,
            _ => Self::Reserved,
        }
    }
}

impl fmt::Display for EntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Available => "available",
            Self::Reserved => "reserved",
            Self::Acpi => "ACPI",
            Self::Nvs => "ACPI NVS",
            Self::Unusable => "unusable",
            Self::Disabled => "unknown (reserved)",
        })
    }
}

/// Memory map entry.
#[derive(Clone)]
struct MapEntry {
    range: Range<PhysAddr>,
    kind: EntryKind,
}

impl MapEntry {
    /// The entry for `length` bytes at `base`, narrowed to whole pages if
    /// available, NVS or disabled; `None` if it wraps around.
    fn new(base: PhysAddr, length: usize, kind: EntryKind) -> Option<Self> {
        let end = base.checked_add(length)?;
        let range = if matches!(
            kind,
            EntryKind::Available | EntryKind::Nvs | EntryKind::Disabled
        ) {
            let start = base.checked_align_up()?;
            let end = end.align_down();
            end.checked_sub(start)?;
            start..end
        } else {
            base..end
        };
        Some(Self { range, kind })
    }
}

/// Boot memory's state.
struct BootMemory {
    /// Boot data, sorted.
    boot_data: ArrayVec<BootData, MAX_BOOT_DATA>,
    /// The memory map.
    map: ArrayVec<MapEntry, { MAX_MAP_ENTRIES * 2 }>,
    /// Input of `normalize_map`.
    map_copy: ArrayVec<MapEntry, MAX_MAP_ENTRIES>,
    /// Physical segments, from the memory map; those after the first
    /// missing one are missing too.
    segments: [Option<Range<PhysAddr>>; Segment::COUNT],
    /// The boot heap's bounds, above low memory, once found.
    heap: Option<Range<PhysAddr>>,
    /// Its part not allocated yet, until `setup` closes it.
    heap_free: Option<Range<PhysAddr>>,
}

static BOOT_MEMORY: BootCell<BootMemory> = BootCell::new(BootMemory {
    boot_data: ArrayVec::new(),
    map: ArrayVec::new(),
    map_copy: ArrayVec::new(),
    segments: [const { None }; Segment::COUNT],
    heap: None,
    heap_free: None,
});

impl BootMemory {
    /// The first part of `range` that avoids all boot data, page-aligned;
    /// `None` if there is none.
    fn find_gap(&self, range: Range<PhysAddr>) -> Option<Range<PhysAddr>> {
        let start = range.start.checked_align_up()?;
        let end = range.end.align_down();
        if start >= end {
            return None;
        }
        self.boot_data
            .iter()
            .try_fold(start..end, |area, boot_data| boot_data.clip(area))
    }

    /// Keeps `range` from the page allocator, as `register_boot_data`.
    fn register(&mut self, range: Range<PhysAddr>, retention: Retention) {
        let Range { start, end } = range;
        assert!(start < end, "boot_memory: invalid boot data");
        let data = &mut self.boot_data;
        assert!(!data.is_full(), "boot_memory: too many boot data ranges");
        // The first entry not wholly below `range`.
        let index = data
            .iter()
            .position(|entry| start < entry.range.end)
            .unwrap_or(data.len());
        if let Some(entry) = data.get_mut(index)
            && end > entry.range.start
        {
            assert!(
                start >= entry.range.start && end <= entry.range.end,
                "boot_memory: invalid boot data"
            );
            // Included: a permanent range stays permanent.
            entry.retention = entry.retention.max(retention);
            return;
        }
        data.insert(
            index,
            BootData {
                range: start..end,
                retention,
            },
        );
    }

    /// Copies the boot loader's memory map, up to `MAX_MAP_ENTRIES` entries;
    /// warns about the rest.
    fn build_map(&mut self, boot_info: Info) {
        let mut built = boot_info.mmap().into_iter().flatten().filter_map(|entry| {
            MapEntry::new(
                PhysAddr(entry.base_addr),
                entry.length,
                EntryKind::from(entry.ty),
            )
        });
        for entry in built.by_ref().take(MAX_MAP_ENTRIES) {
            self.map.push(entry);
        }
        let dropped = built.count();
        if dropped != 0 {
            let _ = write!(
                Serial,
                "boot_memory: warning: {dropped} memory map entries past {MAX_MAP_ENTRIES} ignored\r\n"
            );
        }
    }

    /// Sorts the map, drops empty ranges and resolves overlaps: the most
    /// restrictive kind wins.
    fn normalize_map(&mut self) {
        let (entries, copy) = (&mut self.map, &mut self.map_copy);
        copy.clear();
        for entry in entries.iter().filter(|entry| !entry.range.is_empty()) {
            copy.push(entry.clone());
        }
        entries.clear();
        // Walks from boundary to boundary of the ranges.
        let Some(mut start) = copy.iter().map(|entry| entry.range.start).min() else {
            return;
        };
        while let Some(end) = copy
            .iter()
            .flat_map(|entry| [entry.range.start, entry.range.end])
            .filter(|&point| point > start)
            .min()
        {
            let covering = copy
                .iter()
                .filter(|entry| entry.range.start <= start && end <= entry.range.end);
            if let Some(kind) = covering.map(|entry| entry.kind).max() {
                if let Some(last) = entries.last_mut()
                    && last.range.end == start
                    && last.kind == kind
                {
                    last.range.end = end;
                } else {
                    entries.push(MapEntry {
                        range: start..end,
                        kind,
                    });
                }
            }
            start = end;
        }
    }

    /// Narrows `range` to the available memory it holds: from the first
    /// available page to the end of the last available range starting in it.
    fn available_span(&self, range: Range<PhysAddr>) -> Option<Range<PhysAddr>> {
        let mut span: Option<Range<PhysAddr>> = None;
        for entry in self
            .map
            .iter()
            .filter(|entry| entry.kind == EntryKind::Available)
        {
            let Some(start) = entry.range.start.checked_align_up() else {
                break;
            };
            if start >= range.end {
                break;
            }
            let end = entry.range.end.align_down();
            if start < end && end > range.start {
                span = Some(span.map_or(start, |span| span.start)..end);
            }
        }
        let span = span?;
        Some(span.start.max(range.start)..span.end.min(range.end))
    }

    /// Prints the map.
    fn show_map(&self) {
        let _ = Serial.write_str("boot_memory: physical memory map:\r\n");
        for entry in self.map.iter() {
            let _ = write!(
                Serial,
                "boot_memory: {:018x}:{:018x}, {}\r\n",
                entry.range.start.0, entry.range.end.0, entry.kind
            );
        }
    }

    /// The largest free area of available memory between 1 MiB and `limit`,
    /// avoiding all boot data; `None` if there is none.
    fn largest_gap(&self, limit: PhysAddr) -> Option<Range<PhysAddr>> {
        let mut largest: Option<Range<PhysAddr>> = None;
        for entry in self
            .map
            .iter()
            .filter(|entry| entry.kind == EntryKind::Available)
        {
            let mut start = entry.range.start.max(LOW_MEMORY_END);
            let end = entry.range.end.min(limit);
            while let Some(gap) = self.find_gap(start..end) {
                start = gap.end;
                if largest
                    .as_ref()
                    .is_none_or(|area| gap.end - gap.start > area.end - area.start)
                {
                    largest = Some(gap);
                }
            }
        }
        largest
    }

    /// Takes the largest free area below the kernel window's direct part as
    /// the boot heap.
    ///
    /// # Panics
    ///
    /// If there is none.
    fn init_heap(&mut self) {
        let heap = self
            .largest_gap(DIRECT_MAP_LIMIT)
            .expect("boot_memory: unable to set up the early memory allocator");
        self.heap = Some(heap.clone());
        self.heap_free = Some(heap.clone());
        // Never released to the page allocator.
        self.register(heap, Retention::Permanent);
    }
}

/// Keeps `range` from the page allocator; `Temporary` data is released
/// once no longer needed.
///
/// # Panics
///
/// If the range is empty, the array is full, or the range partly overlaps
/// a registered one.
pub fn register_boot_data(range: Range<PhysAddr>, retention: Retention) {
    BOOT_MEMORY.borrow_mut().register(range, retention);
}

/// Sets up boot memory from the boot information.
///
/// # Panics
///
/// If no memory segment, no AP boot page or no boot heap is found.
pub fn bootstrap(boot_info: Info) {
    let mut memory = BOOT_MEMORY.borrow_mut();
    memory.build_map(boot_info);
    memory.normalize_map();
    let dma = memory
        .available_span(LOW_MEMORY_BASE..DMA_LIMIT)
        .expect("boot_memory: unable to find any memory segment");
    if MAX_CPUS > 1 {
        // Grab an early page for AP boot code, which must be below 1 MiB.
        let gap = memory
            .find_gap(dma.clone())
            .filter(|gap| gap.end - gap.start >= PAGE_SIZE && gap.start < LOW_MEMORY_END)
            .expect("No memory for AP boot code.");
        assert!(
            ap_boot::ADDRESS.set(gap.start).is_ok(),
            "boot_memory: AP boot page set aside twice"
        );
        memory.register(gap.start..gap.start + PAGE_SIZE, Retention::Permanent);
    }
    memory.segments[usize::from(Segment::Dma)] = Some(dma);
    for (segment, range) in [
        (Segment::DirectMap, DMA_LIMIT..DIRECT_MAP_LIMIT),
        (Segment::Dma32, DIRECT_MAP_LIMIT..DMA32_LIMIT),
        (Segment::HighMemory, DMA32_LIMIT..HIGH_MEMORY_LIMIT),
    ] {
        memory.segments[usize::from(segment)] = memory.available_span(range);
    }
    memory.init_heap();
}

/// Takes `count` pages from the top of the boot heap.
///
/// # Panics
///
/// If `count` is 0 or more than the heap has left, or the heap is not open:
/// before `bootstrap` or after `setup`.
pub fn alloc_pages(count: usize) -> PhysAddr {
    assert!(count != 0, "boot_memory: attempt to allocate 0 page");
    let mut memory = BOOT_MEMORY.borrow_mut();
    let free = memory
        .heap_free
        .as_mut()
        .expect("boot_memory: the boot heap is not open");
    let addr = count
        .checked_mul(PAGE_SIZE)
        .and_then(|size| free.end.0.checked_sub(size))
        .map(PhysAddr)
        .filter(|&addr| addr >= free.start)
        .expect("boot_memory: unable to allocate memory");
    free.end = addr;
    addr
}

/// End of the highest available memory.
///
/// # Panics
///
/// Before `bootstrap`.
pub fn memory_end() -> PhysAddr {
    BOOT_MEMORY
        .borrow()
        .segments
        .iter()
        .flatten()
        .map(|segment| segment.end)
        .max()
        .expect("boot_memory: no segment yet")
}

/// End of the boot heap.
///
/// # Panics
///
/// Before `bootstrap`.
pub fn heap_end() -> PhysAddr {
    let memory = BOOT_MEMORY.borrow();
    memory.heap.as_ref().expect("boot_memory: no heap yet").end
}

/// Takes `count` pages for good from the end of the largest free area of
/// all memory, the boot heap's free part among them, then closes the boot
/// heap, whose free part `vm::page` takes; the direct map reaches them.
///
/// # Panics
///
/// If that area is smaller, or the boot heap is not open.
pub fn steal_pages(count: usize) -> PhysAddr {
    let mut memory = BOOT_MEMORY.borrow_mut();
    let size = count * PAGE_SIZE;
    let mut free = memory
        .heap_free
        .take()
        .expect("boot_memory: the boot heap is not open");
    let gap = memory
        .largest_gap(HIGH_MEMORY_LIMIT)
        .filter(|area| area.end - area.start > free.end - free.start);
    let area = gap.clone().unwrap_or_else(|| free.clone());
    assert!(
        area.end - area.start >= size,
        "boot_memory: unable to steal memory"
    );
    let start = PhysAddr(area.end.0 - size);
    if gap.is_some() {
        memory.register(start..area.end, Retention::Permanent);
    } else {
        // The heap is boot data kept for good already.
        free.end = start;
    }
    for Range { start, end } in memory.segments.iter().map_while(Clone::clone) {
        if free.start < end && free.end > start {
            page::load_heap(free.start.max(start)..free.end.min(end));
        }
    }
    start
}

/// Frees usable memory to `vm::page`, on the boot CPU: the temporary boot
/// data is released, then each available area above the first 64 KiB, but
/// for the boot data kept, goes to the page allocator.
pub fn free_usable() {
    let mut memory = BOOT_MEMORY.borrow_mut();
    memory
        .boot_data
        .retain(|data| data.retention == Retention::Permanent);
    for entry in memory
        .map
        .iter()
        .filter(|entry| matches!(entry.kind, EntryKind::Available))
    {
        let end = entry.range.end.min(HIGH_MEMORY_LIMIT);
        let mut start = entry.range.start.max(LOW_MEMORY_BASE);
        while let Some(gap) = memory.find_gap(start..end) {
            start = gap.end;
            page::release(gap);
        }
    }
}

/// Shows the physical memory map and loads its segments into `vm::page`.
pub fn setup() {
    let memory = BOOT_MEMORY.borrow();
    memory.show_map();
    for Range { start, end } in memory.segments.iter().map_while(Clone::clone) {
        page::load(start..end);
    }
}
