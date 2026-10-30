// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Boot-time physical memory.

use core::cell::UnsafeCell;
use core::ops::Range;
use core::sync::atomic::Ordering;

use super::address::PhysAddr;
use super::ap_boot;
use super::memory_layout::{
    DIRECT_MAP_LIMIT, DMA_LIMIT, DMA32_LIMIT, HIGH_MEMORY_LIMIT, PAGE_SIZE, Segment,
};
use crate::boot::multiboot2::{Info, MemoryType};
use crate::config::MAX_CPUS;

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
#[derive(Clone, Copy)]
struct BootData {
    start: PhysAddr,
    end: PhysAddr,
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
        let start = self.start.align_down();
        let end = self
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

/// Sorted array of boot data descriptors.
struct BootDataArray {
    data: UnsafeCell<[BootData; MAX_BOOT_DATA]>,
    len: UnsafeCell<usize>,
}

// SAFETY: only the boot CPU touches it.
unsafe impl Sync for BootDataArray {}

impl BootDataArray {
    /// The first part of `range` that avoids all boot data, page-aligned;
    /// `None` if there is none.
    ///
    /// # Safety
    ///
    /// Runs on the boot CPU only.
    unsafe fn find_gap(&self, range: Range<PhysAddr>) -> Option<Range<PhysAddr>> {
        let start = range.start.checked_align_up()?;
        let end = range.end.align_down();
        if start >= end {
            return None;
        }
        // SAFETY: guaranteed by the caller.
        let (data, len) = unsafe { (&*self.data.get(), *self.len.get()) };
        data[..len]
            .iter()
            .try_fold(start..end, |area, boot_data| boot_data.clip(area))
    }
}

static BOOT_DATA: BootDataArray = BootDataArray {
    data: UnsafeCell::new(
        [BootData {
            start: PhysAddr(0),
            end: PhysAddr(0),
            retention: Retention::Permanent,
        }; MAX_BOOT_DATA],
    ),
    len: UnsafeCell::new(0),
};

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

/// Memory map entry.
#[derive(Clone, Copy)]
struct MapEntry {
    base_addr: PhysAddr,
    length: u64,
    kind: EntryKind,
}

impl MapEntry {
    /// End of the range, past its last byte.
    fn end(&self) -> PhysAddr {
        self.base_addr + self.length
    }

    /// Narrows available, NVS and disabled ranges to whole pages; `None` if
    /// the range wraps around.
    fn narrow_to_pages(mut self) -> Option<Self> {
        let end = self.base_addr.checked_add(self.length)?;
        if matches!(
            self.kind,
            EntryKind::Available | EntryKind::Nvs | EntryKind::Disabled
        ) {
            self.base_addr = self.base_addr.checked_align_up()?;
            self.length = end.align_down().checked_sub(self.base_addr)?;
        }
        Some(self)
    }
}

/// The memory map.
struct Map {
    entries: UnsafeCell<[MapEntry; MAX_MAP_ENTRIES * 2]>,
    len: UnsafeCell<usize>,
    /// Input of `normalize`.
    copy: UnsafeCell<[MapEntry; MAX_MAP_ENTRIES]>,
}

// SAFETY: only the boot CPU touches it.
unsafe impl Sync for Map {}

impl Map {
    /// Copies the boot loader's memory map, up to `MAX_MAP_ENTRIES` entries.
    ///
    /// # Safety
    ///
    /// Runs on the boot CPU only.
    unsafe fn build(&self, boot_info: Info) {
        // SAFETY: guaranteed by the caller.
        let (entries, len) = unsafe { (&mut *self.entries.get(), &mut *self.len.get()) };
        let built = boot_info.mmap().into_iter().flatten().filter_map(|entry| {
            MapEntry {
                base_addr: PhysAddr(entry.base_addr),
                length: entry.length,
                kind: EntryKind::from(entry.ty),
            }
            .narrow_to_pages()
        });
        for (slot, entry) in entries[..MAX_MAP_ENTRIES].iter_mut().zip(built) {
            *slot = entry;
            *len += 1;
        }
    }

    /// Sorts the map, drops empty ranges and resolves overlaps: the most
    /// restrictive kind wins.
    ///
    /// # Safety
    ///
    /// Runs on the boot CPU only.
    unsafe fn normalize(&self) {
        // SAFETY: guaranteed by the caller.
        let (entries, len, copy) = unsafe {
            (
                &mut *self.entries.get(),
                &mut *self.len.get(),
                &mut *self.copy.get(),
            )
        };
        let mut n = 0;
        for entry in &entries[..*len] {
            if entry.length != 0 {
                copy[n] = *entry;
                n += 1;
            }
        }
        let copy = &copy[..n];
        *len = 0;
        // Walks from boundary to boundary of the ranges.
        let Some(mut start) = copy.iter().map(|entry| entry.base_addr).min() else {
            return;
        };
        while let Some(end) = copy
            .iter()
            .flat_map(|entry| [entry.base_addr, entry.end()])
            .filter(|&point| point > start)
            .min()
        {
            let covering = copy
                .iter()
                .filter(|entry| entry.base_addr <= start && end <= entry.end());
            if let Some(kind) = covering.map(|entry| entry.kind).max() {
                if let Some(last) = entries[..*len].last_mut()
                    && last.end() == start
                    && last.kind == kind
                {
                    last.length += end - start;
                } else {
                    entries[*len] = MapEntry {
                        base_addr: start,
                        length: end - start,
                        kind,
                    };
                    *len += 1;
                }
            }
            start = end;
        }
    }

    /// Narrows `range` to the available memory it holds: from the first
    /// available page to the end of the last available range starting in it.
    ///
    /// # Safety
    ///
    /// Runs on the boot CPU only.
    unsafe fn available_span(&self, range: Range<PhysAddr>) -> Option<Range<PhysAddr>> {
        // SAFETY: guaranteed by the caller.
        let (entries, len) = unsafe { (&*self.entries.get(), *self.len.get()) };
        let mut span: Option<Range<PhysAddr>> = None;
        for entry in entries[..len]
            .iter()
            .filter(|entry| entry.kind == EntryKind::Available)
        {
            let Some(start) = entry.base_addr.checked_align_up() else {
                break;
            };
            if start >= range.end {
                break;
            }
            let end = entry.end().align_down();
            if start < end && end > range.start {
                span = Some(span.map_or(start, |span| span.start)..end);
            }
        }
        let span = span?;
        Some(span.start.max(range.start)..span.end.min(range.end))
    }
}

static MAP: Map = Map {
    entries: UnsafeCell::new(
        [MapEntry {
            base_addr: PhysAddr(0),
            length: 0,
            kind: EntryKind::Available,
        }; MAX_MAP_ENTRIES * 2],
    ),
    len: UnsafeCell::new(0),
    copy: UnsafeCell::new(
        [MapEntry {
            base_addr: PhysAddr(0),
            length: 0,
            kind: EntryKind::Available,
        }; MAX_MAP_ENTRIES],
    ),
};

/// Physical segments, from the memory map.
struct Segments(UnsafeCell<[Range<PhysAddr>; Segment::COUNT]>);

// SAFETY: only the boot CPU touches it.
unsafe impl Sync for Segments {}

impl Segments {
    /// Sets the bounds of `segment`.
    ///
    /// # Safety
    ///
    /// Runs on the boot CPU only.
    unsafe fn set(&self, segment: Segment, range: Range<PhysAddr>) {
        // SAFETY: guaranteed by the caller.
        unsafe { (*self.0.get())[usize::from(segment)] = range };
    }
}

static SEGMENTS: Segments = Segments(UnsafeCell::new(
    [const { PhysAddr(0)..PhysAddr(0) }; Segment::COUNT],
));

/// The boot heap, above low memory.
struct Heap {
    /// Its bounds.
    range: UnsafeCell<Range<PhysAddr>>,
    /// Its part not allocated yet.
    free: UnsafeCell<Range<PhysAddr>>,
}

// SAFETY: only the boot CPU touches it.
unsafe impl Sync for Heap {}

impl Heap {
    /// Takes the largest free area of available memory between 1 MiB and the
    /// direct map's end as the boot heap, avoiding all boot data.
    ///
    /// # Panics
    ///
    /// If there is none.
    ///
    /// # Safety
    ///
    /// Runs on the boot CPU only.
    unsafe fn init(&self) {
        // SAFETY: guaranteed by the caller.
        let (entries, len) = unsafe { (&*MAP.entries.get(), *MAP.len.get()) };
        let mut heap = PhysAddr(0)..PhysAddr(0);
        for entry in entries[..len]
            .iter()
            .filter(|entry| entry.kind == EntryKind::Available)
        {
            let mut start = entry.base_addr.max(LOW_MEMORY_END);
            let end = entry.end().min(DIRECT_MAP_LIMIT);
            // SAFETY: guaranteed by the caller.
            while let Some(gap) = unsafe { BOOT_DATA.find_gap(start..end) } {
                start = gap.end;
                if gap.end - gap.start > heap.end - heap.start {
                    heap = gap;
                }
            }
        }
        assert!(
            heap.start < heap.end,
            "boot_memory: unable to set up the early memory allocator"
        );
        // SAFETY: guaranteed by the caller.
        unsafe {
            *self.range.get() = heap.clone();
            *self.free.get() = heap.clone();
            // Never released to the page allocator.
            register_boot_data(heap, Retention::Permanent);
        }
    }
}

static HEAP: Heap = Heap {
    range: UnsafeCell::new(PhysAddr(0)..PhysAddr(0)),
    free: UnsafeCell::new(PhysAddr(0)..PhysAddr(0)),
};

/// Keeps `range` from the page allocator; `Temporary` data is released
/// once no longer needed.
///
/// # Panics
///
/// If the range is empty, the array is full, or the range partly overlaps
/// a registered one.
///
/// # Safety
///
/// Runs on the boot CPU only.
pub unsafe fn register_boot_data(range: Range<PhysAddr>, retention: Retention) {
    let Range { start, end } = range;
    assert!(start < end, "boot_memory: invalid boot data");
    // SAFETY: guaranteed by the caller.
    let (data, len) = unsafe { (&mut *BOOT_DATA.data.get(), &mut *BOOT_DATA.len.get()) };
    assert!(*len < data.len(), "boot_memory: too many boot data ranges");
    let mut i = 0;
    while i < *len {
        let entry = &mut data[i];
        if end > entry.start && start < entry.end {
            assert!(
                start >= entry.start && end <= entry.end,
                "boot_memory: invalid boot data"
            );
            // Included: a permanent range stays permanent.
            entry.retention = entry.retention.max(retention);
            return;
        }
        if end <= entry.start {
            break;
        }
        i += 1;
    }
    data.copy_within(i..*len, i + 1);
    data[i] = BootData {
        start,
        end,
        retention,
    };
    *len += 1;
}

/// Sets up boot memory from the boot information.
///
/// # Safety
///
/// Runs once, on the boot CPU.
pub unsafe fn bootstrap(boot_info: Info) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        MAP.build(boot_info);
        MAP.normalize();
    }
    // SAFETY: guaranteed by the caller.
    let dma = unsafe { MAP.available_span(LOW_MEMORY_BASE..DMA_LIMIT) }
        .expect("boot_memory: unable to find any memory segment");
    if MAX_CPUS > 1 {
        // Grab an early page for AP boot code, which must be below 1 MiB.
        // SAFETY: guaranteed by the caller.
        let gap = unsafe { BOOT_DATA.find_gap(dma.clone()) }
            .filter(|gap| gap.end - gap.start >= PAGE_SIZE && gap.start < LOW_MEMORY_END)
            .expect("No memory for AP boot code.");
        ap_boot::ADDRESS.store(gap.start.0, Ordering::Relaxed);
        // SAFETY: guaranteed by the caller.
        unsafe { register_boot_data(gap.start..gap.start + PAGE_SIZE, Retention::Permanent) };
    }
    // SAFETY: guaranteed by the caller.
    unsafe { SEGMENTS.set(Segment::Dma, dma) };
    for (segment, range) in [
        (Segment::DirectMap, DMA_LIMIT..DIRECT_MAP_LIMIT),
        (Segment::Dma32, DIRECT_MAP_LIMIT..DMA32_LIMIT),
        (Segment::HighMemory, DMA32_LIMIT..HIGH_MEMORY_LIMIT),
    ] {
        // SAFETY: guaranteed by the caller.
        if let Some(found) = unsafe { MAP.available_span(range) } {
            // SAFETY: guaranteed by the caller.
            unsafe { SEGMENTS.set(segment, found) };
        }
    }
    // SAFETY: guaranteed by the caller.
    unsafe { HEAP.init() };
}
