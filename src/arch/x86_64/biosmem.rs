// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Boot-time physical memory.

use core::cell::UnsafeCell;

const BIOSMEM_MAX_BOOT_DATA: usize = 64;

/// Boot data descriptor.
#[derive(Clone, Copy)]
struct BootData {
    start: u64,
    end: u64,
    temporary: bool,
}

/// Sorted array of boot data descriptors.
struct BootDataArray {
    data: UnsafeCell<[BootData; BIOSMEM_MAX_BOOT_DATA]>,
    len: UnsafeCell<usize>,
}

// SAFETY: only the boot CPU touches it.
unsafe impl Sync for BootDataArray {}

static BOOT_DATA: BootDataArray = BootDataArray {
    data: UnsafeCell::new(
        [BootData {
            start: 0,
            end: 0,
            temporary: false,
        }; BIOSMEM_MAX_BOOT_DATA],
    ),
    len: UnsafeCell::new(0),
};

/// Keeps `start..end` from the page allocator; `temporary` data is
/// released once no longer needed.
///
/// # Panics
///
/// If the range is empty, the array is full, or the range partly overlaps
/// a registered one.
///
/// # Safety
///
/// Runs on the boot CPU only.
pub unsafe fn register_boot_data(start: u64, end: u64, temporary: bool) {
    assert!(start < end, "biosmem: invalid boot data");
    // SAFETY: guaranteed by the caller.
    let (data, len) = unsafe { (&mut *BOOT_DATA.data.get(), &mut *BOOT_DATA.len.get()) };
    assert!(*len < data.len(), "biosmem: too many boot data ranges");
    let mut i = 0;
    while i < *len {
        let entry = &mut data[i];
        if end > entry.start && start < entry.end {
            assert!(
                start >= entry.start && end <= entry.end,
                "biosmem: invalid boot data"
            );
            // Included: a permanent range stays permanent.
            entry.temporary &= temporary;
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
        temporary,
    };
    *len += 1;
}
