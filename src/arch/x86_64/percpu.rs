// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! x86_64 per-CPU data.

use x86_64::structures::gdt::GlobalDescriptorTable;

use super::gdt;
use super::idt::Gate;
use crate::cell::SyncUnsafeCell;

/// One CPU's machine-dependent state.
pub struct PerCpu {
    /// This CPU's GDT.
    pub(super) gdt: GlobalDescriptorTable,
    /// This CPU's IDT; filled at runtime.
    pub(super) idt: SyncUnsafeCell<[Gate; 256]>,
}

impl PerCpu {
    pub const fn new() -> Self {
        Self {
            gdt: gdt::new(),
            idt: SyncUnsafeCell::new([const { Gate::missing() }; 256]),
        }
    }
}
