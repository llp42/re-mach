// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The global descriptor table.

use x86_64::PrivilegeLevel;
use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::tss::TaskStateSegment;

/// Entries, in GNU Mach's layout with the user segments and the TSS in its
/// LDTs' slots; the user TLS slots, 9 and 10, are the last its interface
/// shows.
pub const ENTRIES: usize = 14;

/// Kernel code selector.
pub const KERNEL_CODE: SegmentSelector = SegmentSelector::new(1, PrivilegeLevel::Ring0);

/// Kernel data selector.
pub const KERNEL_DATA: SegmentSelector = SegmentSelector::new(2, PrivilegeLevel::Ring0);

/// User data selector.
pub const USER_DATA: SegmentSelector = SegmentSelector::new(3, PrivilegeLevel::Ring3);

/// User code selector.
pub const USER_CODE: SegmentSelector = SegmentSelector::new(4, PrivilegeLevel::Ring3);

/// The TSS's selector; its descriptor takes two entries.
const KERNEL_TSS: SegmentSelector = SegmentSelector::new(5, PrivilegeLevel::Ring0);

/// Fills `table` with the kernel code and data segments, loads it and
/// reloads CS, SS, DS and ES from it; FS and GS stay null, keeping the
/// per-CPU GS base.
///
/// # Safety
///
/// Runs once, on the CPU whose table `table` is, before any segment
/// register loads from it.
pub unsafe fn init(table: &'static GlobalDescriptorTable<ENTRIES>) {
    // SAFETY: no segment register was loaded from the table.
    unsafe {
        table.store(
            usize::from(KERNEL_CODE.index()),
            Descriptor::kernel_code_segment(),
        );
        table.store(
            usize::from(KERNEL_DATA.index()),
            Descriptor::kernel_data_segment(),
        );
    }
    table.load();
    // SAFETY: the selectors name the descriptors just loaded.
    unsafe {
        CS::set_reg(KERNEL_CODE);
        SS::set_reg(KERNEL_DATA);
        DS::set_reg(KERNEL_DATA);
        ES::set_reg(KERNEL_DATA);
    }
}

/// Writes the user data and code segments into `table`.
///
/// # Safety
///
/// Runs once, on the CPU whose table `table` is, after `init`.
pub unsafe fn store_user_segments(table: &GlobalDescriptorTable<ENTRIES>) {
    // SAFETY: no segment register was loaded from these slots.
    unsafe {
        table.store(
            usize::from(USER_DATA.index()),
            Descriptor::user_data_segment(),
        );
        table.store(
            usize::from(USER_CODE.index()),
            Descriptor::user_code_segment(),
        );
    }
}

/// Writes `tss`'s descriptor into `table`, returning its selector.
///
/// # Safety
///
/// Runs once, on the CPU whose table `table` is, after `init`; `tss` is
/// that CPU's, in its per-CPU data.
pub unsafe fn store_tss(
    table: &GlobalDescriptorTable<ENTRIES>,
    tss: *const TaskStateSegment,
) -> SegmentSelector {
    // SAFETY: guaranteed by the caller; TR was never loaded from these
    // slots.
    unsafe {
        table.store(
            usize::from(KERNEL_TSS.index()),
            Descriptor::tss_segment_unchecked(tss),
        )
    }
}
