// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Boot-time physical memory.

use core::cell::UnsafeCell;
use core::ops::Range;

use super::address::PhysAddr;
use crate::boot::{BootInfo, MemoryKind};

const MAX_BOOT_DATA: usize = 64;

/// How long boot data is kept.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Retention {
    /// Released once no longer needed.
    #[expect(dead_code, reason = "no boot data is temporary yet")]
    Temporary,
    /// Kept for good.
    Permanent,
}

/// Boot data descriptor.
#[derive(Clone, Copy)]
struct BootData {
    start: u64,
    end: u64,
    retention: Retention,
}

/// Sorted array of boot data descriptors.
struct BootDataArray {
    data: UnsafeCell<[BootData; MAX_BOOT_DATA]>,
    len: UnsafeCell<usize>,
}

// SAFETY: only the boot CPU touches it.
unsafe impl Sync for BootDataArray {}

static BOOT_DATA: BootDataArray = BootDataArray {
    data: UnsafeCell::new(
        [BootData {
            start: 0,
            end: 0,
            retention: Retention::Permanent,
        }; MAX_BOOT_DATA],
    ),
    len: UnsafeCell::new(0),
};

/// Entries kept from the boot loader's map; overlap resolution can double them.
const MAX_MAP_ENTRIES: usize = 128;

/// Memory range kinds; later ones are more restrictive and win overlaps.
#[derive(Clone, Copy)]
enum EntryKind {
    Available,
    Reserved,
    Acpi,
    Nvs,
    Unusable,
    #[expect(dead_code, reason = "no boot source reports it")]
    Disabled,
}

impl From<MemoryKind> for EntryKind {
    fn from(kind: MemoryKind) -> Self {
        match kind {
            MemoryKind::Available => Self::Available,
            MemoryKind::Reserved => Self::Reserved,
            MemoryKind::AcpiReclaimable => Self::Acpi,
            MemoryKind::Nvs => Self::Nvs,
            MemoryKind::BadRam => Self::Unusable,
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
}

// SAFETY: only the boot CPU touches it.
unsafe impl Sync for Map {}

impl Map {
    /// Copies the boot loader's memory map, up to `MAX_MAP_ENTRIES` entries.
    ///
    /// # Safety
    ///
    /// Runs on the boot CPU only.
    unsafe fn build(&self, boot_info: &impl BootInfo) {
        // SAFETY: guaranteed by the caller.
        let (entries, len) = unsafe { (&mut *self.entries.get(), &mut *self.len.get()) };
        let built = boot_info.memory_map().filter_map(|region| {
            MapEntry {
                base_addr: PhysAddr(region.address),
                length: region.length,
                kind: EntryKind::from(region.kind),
            }
            .narrow_to_pages()
        });
        for (slot, entry) in entries[..MAX_MAP_ENTRIES].iter_mut().zip(built) {
            *slot = entry;
            *len += 1;
        }
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
pub unsafe fn register_boot_data(range: Range<u64>, retention: Retention) {
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
pub unsafe fn bootstrap(boot_info: &impl BootInfo) {
    // SAFETY: guaranteed by the caller.
    unsafe { MAP.build(boot_info) };
}
