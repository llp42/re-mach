// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Global descriptor tables, one per CPU.

use x86_64::PrivilegeLevel;
use x86_64::instructions::segmentation::{CS, DS, ES, FS, GS, SS, Segment};
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};

use crate::smp::PerCpu;

/// Null, kernel code, kernel data.
pub const fn new() -> GlobalDescriptorTable {
    let mut gdt = GlobalDescriptorTable::new();
    gdt.append(Descriptor::kernel_code_segment());
    gdt.append(Descriptor::kernel_data_segment());
    gdt
}

/// Loads `per_cpu`'s GDT and reloads the segment registers.
pub fn init(per_cpu: &'static PerCpu) {
    per_cpu.md.gdt.load();
    let code = SegmentSelector::new(1, PrivilegeLevel::Ring0);
    let data = SegmentSelector::new(2, PrivilegeLevel::Ring0);
    // SAFETY: entries 1 and 2 are kernel code and data; interrupts are off.
    unsafe {
        CS::set_reg(code);
        SS::set_reg(data);
        DS::set_reg(data);
        ES::set_reg(data);
        FS::set_reg(data);
        GS::set_reg(data);
    }
}
