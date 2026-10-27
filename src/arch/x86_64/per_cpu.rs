// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! `x86_64` per-CPU data.

use core::ptr;

use super::cpu::{self, Cpuid};
use super::gdt::{self, Gdt};
use super::idt::{self, Idt};
use super::instructions;
use super::tss::{self, Stack, Tss};
use crate::smp;

/// One CPU's machine-dependent state.
pub struct PerCpu {
    /// This CPU's GDT.
    pub(super) gdt: Gdt,
    /// This CPU's IDT; filled at runtime.
    pub(super) idt: Idt,
    /// This CPU's CPUID results.
    pub(super) cpuid: Cpuid,
    /// This CPU's TSS.
    pub(super) tss: Tss,
    /// IST stack for NMI and #DF.
    pub(super) double_fault_stack: Stack,
    /// IST stack for #DB.
    pub(super) debug_stack: Stack,
    /// IST stack for #MC.
    pub(super) machine_check_stack: Stack,
}

impl PerCpu {
    pub const fn new() -> Self {
        Self {
            gdt: Gdt::new(),
            idt: Idt::new(),
            cpuid: Cpuid::new(),
            tss: Tss::new(),
            double_fault_stack: Stack::new(),
            debug_stack: Stack::new(),
            machine_check_stack: Stack::new(),
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
    tss::init(per_cpu);
    idt::init(per_cpu);
    cpu::identify(per_cpu);
    cpu::initialize(per_cpu);
}
