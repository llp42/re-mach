// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data, reached through the GS base.

use core::arch::asm;
use core::cell::UnsafeCell;
use core::mem::offset_of;

use x86_64::VirtAddr;
use x86_64::registers::model_specific::{GsBase, KernelGsBase};

use crate::config::MAX_CPUS;
use crate::smp;

/// One CPU's data; the GS base points at the current CPU's.
#[repr(C)]
pub struct PerCpu {
    id: usize,
}

struct Slot(UnsafeCell<PerCpu>);

// SAFETY: each CPU writes only its own slot, in `init`.
unsafe impl Sync for Slot {}

static PER_CPU: [Slot; MAX_CPUS] = [const { Slot(UnsafeCell::new(PerCpu { id: 0 })) }; MAX_CPUS];

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
