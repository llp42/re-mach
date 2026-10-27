// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! x86_64 per-CPU data.

use x86_64::structures::gdt::GlobalDescriptorTable;

use super::gdt;

/// One CPU's machine-dependent state.
pub struct PerCpu {
    /// This CPU's GDT.
    pub(super) gdt: GlobalDescriptorTable,
}

impl PerCpu {
    pub const fn new() -> Self {
        Self { gdt: gdt::new() }
    }
}
