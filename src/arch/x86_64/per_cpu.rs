// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data, reached through the GS base.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::offset_of;

use raw_cpuid::CpuId;
use x86_64::VirtAddr;
use x86_64::registers::model_specific::{GsBase, KernelGsBase};
use x86_64::structures::gdt::GlobalDescriptorTable;
use x86_64::structures::tss::TaskStateSegment;

use super::gdt;
use crate::config::MAX_CPUS;
use crate::smp::{self, PerCpu as _};

/// One CPU's data; the GS base points at the current CPU's.
#[repr(C)]
pub struct PerCpu {
    id: usize,
    apic_id: u8,
    /// Its GDT, which holds its TSS's descriptor.
    gdt: Option<&'static GlobalDescriptorTable<{ gdt::ENTRIES }>>,
    tss: TaskStateSegment,
}

struct Slot(UnsafeCell<PerCpu>);

// SAFETY: each CPU writes only its own slot.
unsafe impl Sync for Slot {}

static PER_CPU: [Slot; MAX_CPUS] = [const {
    Slot(UnsafeCell::new(PerCpu {
        id: 0,
        apic_id: 0,
        gdt: None,
        tss: TaskStateSegment::new(),
    }))
}; MAX_CPUS];

/// Fills CPU `cpu_id`'s data and points the GS base at it.
///
/// # Safety
///
/// Runs first on CPU `cpu_id`, and only there.
pub unsafe fn init(cpu_id: usize) {
    let per_cpu = PER_CPU[cpu_id].0.get();
    // SAFETY: only CPU `cpu_id` touches its slot.
    unsafe { (*per_cpu).id = cpu_id };
    // SAFETY: GS holds nothing else.
    unsafe { GsBase::write(VirtAddr::from_ptr(per_cpu)) };
    // SAFETY: user GS base, for `swapgs`; none yet.
    unsafe { KernelGsBase::write(VirtAddr::zero()) };
}

/// Stores the current CPU's local APIC ID, from CPUID, in its data.
///
/// # Safety
///
/// Runs once on each CPU, after `init`.
pub unsafe fn store_apic_id() {
    let apic_id = CpuId::new()
        .get_feature_info()
        .expect("CPUID leaf 1")
        .initial_local_apic_id();
    let per_cpu = PER_CPU[PerCpu::cpu_id()].0.get();
    // SAFETY: only this CPU touches its slot.
    unsafe { (*per_cpu).apic_id = apic_id };
}

/// Stores the current CPU's GDT in its data.
///
/// # Safety
///
/// Runs once on each CPU, after `init`.
pub unsafe fn store_gdt(table: &'static GlobalDescriptorTable<{ gdt::ENTRIES }>) {
    let per_cpu = PER_CPU[PerCpu::cpu_id()].0.get();
    // SAFETY: only this CPU touches its slot.
    unsafe { (*per_cpu).gdt = Some(table) };
}

/// The current CPU's TSS.
pub fn tss() -> *mut TaskStateSegment {
    let per_cpu = PER_CPU[PerCpu::cpu_id()].0.get();
    // SAFETY: a field of this CPU's slot; no reference is made.
    unsafe { &raw mut (*per_cpu).tss }
}

impl smp::PerCpu for PerCpu {
    fn cpu_id() -> usize {
        let id;
        // SAFETY: `init` ran first on this CPU; `id` is a `usize`.
        unsafe {
            asm!(
                "movq %gs:{offset}, {id}",
                offset = const offset_of!(Self, id),
                id = out(reg) id,
                options(att_syntax, nostack, preserves_flags, readonly),
            );
        }
        id
    }
}
