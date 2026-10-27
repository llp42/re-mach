// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! `x86_64` per-CPU data.

use super::gdt::Gdt;
use super::idt::Idt;

/// One CPU's machine-dependent state.
pub struct PerCpu {
    /// This CPU's GDT.
    pub(super) gdt: Gdt,
    /// This CPU's IDT; filled at runtime.
    pub(super) idt: Idt,
}

impl PerCpu {
    pub const fn new() -> Self {
        Self {
            gdt: Gdt::new(),
            idt: Idt::new(),
        }
    }
}
