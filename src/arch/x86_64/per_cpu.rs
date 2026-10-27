// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! `x86_64` per-CPU data.

use core::ptr;

use super::cpu::{self, Cpuid};
use super::gdt::{self, Gdt};
use super::idt::{self, Idt};
use super::instructions;
use crate::smp;

/// One CPU's machine-dependent state.
pub struct PerCpu {
    /// This CPU's GDT.
    pub(super) gdt: Gdt,
    /// This CPU's IDT; filled at runtime.
    pub(super) idt: Idt,
    /// This CPU's CPUID results.
    pub(super) cpuid: Cpuid,
}

impl PerCpu {
    pub const fn new() -> Self {
        Self {
            gdt: Gdt::new(),
            idt: Idt::new(),
            cpuid: Cpuid::new(),
        }
    }
}

/// Sets up the CPU running this, with its `per_cpu` area.
pub fn init(per_cpu: &'static smp::PerCpu) {
    gdt::init(per_cpu);
    // SAFETY: nothing reads GS yet.
    unsafe {
        instructions::write_msr(
            instructions::IA32_GS_BASE,
            u64::try_from(ptr::from_ref(per_cpu).expose_provenance()).unwrap(),
        );
    }
    idt::init(per_cpu);
    cpu::identify(per_cpu);
    cpu::initialize(per_cpu);
}
