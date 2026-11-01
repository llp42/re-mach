// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The global descriptor table.

use x86_64::PrivilegeLevel;
use x86_64::instructions::segmentation::{CS, DS, ES, SS, Segment};
use x86_64::structures::gdt::{DescriptorFlags, GlobalDescriptorTable, SegmentSelector};

/// Entries, in GNU Mach's layout; the user TLS slots, 9 and 10, are the
/// last its interface shows.
const ENTRIES: usize = 14;

/// Kernel code selector.
const KERNEL_CODE: SegmentSelector = SegmentSelector::new(1, PrivilegeLevel::Ring0);

/// Kernel data selector.
const KERNEL_DATA: SegmentSelector = SegmentSelector::new(2, PrivilegeLevel::Ring0);

/// The boot CPU's.
static GDT: GlobalDescriptorTable<ENTRIES> = GlobalDescriptorTable::from_raw_entries(&[
    0,
    DescriptorFlags::KERNEL_CODE64.bits(),
    DescriptorFlags::KERNEL_DATA.bits(),
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
]);

/// Loads the boot CPU's GDT and reloads CS, SS, DS and ES from it; FS and
/// GS stay null, keeping the per-CPU GS base.
///
/// # Safety
///
/// Runs once, on the boot CPU.
pub unsafe fn init() {
    GDT.load();
    // SAFETY: the selectors name the descriptors just loaded.
    unsafe {
        CS::set_reg(KERNEL_CODE);
        SS::set_reg(KERNEL_DATA);
        DS::set_reg(KERNEL_DATA);
        ES::set_reg(KERNEL_DATA);
    }
}
