// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data, reached through the GS base: the machine's part.

use core::arch::asm;
use core::cell::Cell;
use core::mem::{self, MaybeUninit, offset_of};
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU64, Ordering};

use raw_cpuid::CpuId;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::registers::model_specific::{GsBase, KernelGsBase};
use x86_64::structures::gdt::GlobalDescriptorTable;
use x86_64::structures::tss::TaskStateSegment;

use super::{gdt, interrupt};
use crate::config::MAX_CPUS;
use crate::smp::{self, MachinePerCpu};
use crate::sync::OnceLock;

/// One CPU's machine-dependent data, holding its machine-independent part;
/// the GS base points at the current CPU's.
pub struct PerCpu {
    /// Itself, read through the GS base; set by `init` before the GS base
    /// points here.
    this: AtomicPtr<Self>,
    common: smp::PerCpu,
    /// Its local APIC's ID.
    apic_id: OnceLock<u8>,
    /// Its GDT, which holds its TSS's descriptor.
    gdt: OnceLock<&'static GlobalDescriptorTable<{ gdt::ENTRIES }>>,
    tss: Cell<TaskStateSegment>,
    /// The interrupts its critical sections held back, a bit per vector.
    pending_interrupts: [AtomicU64; 4],
}

// SAFETY: only its own CPU touches the TSS.
unsafe impl Sync for PerCpu {}

/// Each CPU's, by number.
static PER_CPU: [PerCpu; MAX_CPUS] = {
    let mut slots = [const { MaybeUninit::<PerCpu>::uninit() }; MAX_CPUS];
    let mut id = 0;
    while id < MAX_CPUS {
        slots[id] = MaybeUninit::new(PerCpu::new(id));
        id += 1;
    }
    // SAFETY: every slot is written.
    unsafe { mem::transmute(slots) }
};

/// Points the GS base at CPU `cpu_id`'s data.
///
/// # Safety
///
/// Runs first on CPU `cpu_id`, and only there.
pub unsafe fn init(cpu_id: usize) {
    let slot = &PER_CPU[cpu_id];
    slot.this
        .store(ptr::from_ref(slot).cast_mut(), Ordering::Relaxed);
    // SAFETY: GS holds nothing else.
    unsafe { GsBase::write(VirtAddr::from_ptr(slot)) };
    // SAFETY: user GS base, for `swapgs`; none yet.
    unsafe { KernelGsBase::write(VirtAddr::zero()) };
}

impl PerCpu {
    /// CPU `id`'s, before it starts.
    const fn new(id: usize) -> Self {
        Self {
            this: AtomicPtr::new(ptr::null_mut()),
            common: smp::PerCpu::new(id),
            apic_id: OnceLock::new(),
            gdt: OnceLock::new(),
            tss: Cell::new(TaskStateSegment::new()),
            pending_interrupts: [const { AtomicU64::new(0) }; 4],
        }
    }

    /// Stores the current CPU's local APIC ID, from CPUID.
    ///
    /// # Panics
    ///
    /// On a second call on this CPU.
    pub fn store_apic_id() {
        let apic_id = CpuId::new()
            .get_feature_info()
            .expect("CPUID leaf 1")
            .initial_local_apic_id();
        assert!(
            Self::current().apic_id.set(apic_id).is_ok(),
            "per_cpu: APIC ID stored twice"
        );
    }

    /// Stores its GDT.
    ///
    /// # Panics
    ///
    /// On a second call.
    pub fn store_gdt(&self, table: &'static GlobalDescriptorTable<{ gdt::ENTRIES }>) {
        assert!(self.gdt.set(table).is_ok(), "per_cpu: GDT stored twice");
    }

    /// Its TSS, which stays where it is.
    pub const fn tss(&self) -> &Cell<TaskStateSegment> {
        &self.tss
    }

    /// Holds `vector` back if this CPU is in a critical section, returning
    /// whether it did; with interrupts off.
    pub fn hold(&self, vector: usize) -> bool {
        if !self.common.in_critical_section() {
            return false;
        }
        let word = &self.pending_interrupts[vector / 64];
        word.store(
            word.load(Ordering::Relaxed) | 1 << (vector % 64),
            Ordering::Relaxed,
        );
        true
    }

    /// Takes this CPU's lowest pending vector, if any; with interrupts off.
    fn take_pending(&self) -> Option<usize> {
        self.pending_interrupts
            .iter()
            .enumerate()
            .find_map(|(index, word)| {
                let bits = word.load(Ordering::Relaxed);
                (bits != 0).then(|| {
                    let bit = bits.trailing_zeros();
                    word.store(bits & !(1 << bit), Ordering::Relaxed);
                    index * 64 + usize::try_from(bit).unwrap()
                })
            })
    }
}

impl MachinePerCpu for PerCpu {
    fn current() -> &'static Self {
        let this: *const Self;
        // SAFETY: `init` ran first on this CPU, pointing the GS base at its
        // slot, whose `this` names it.
        unsafe {
            asm!(
                "movq %gs:{offset}, {this}",
                offset = const offset_of!(Self, this),
                this = out(reg) this,
                options(att_syntax, nostack, preserves_flags, readonly),
            );
            &*this
        }
    }

    fn common(&self) -> &smp::PerCpu {
        &self.common
    }

    fn run_pending_interrupts(&self) {
        while let Some(vector) = interrupts::without_interrupts(|| self.take_pending()) {
            self.common.enter_critical();
            interrupt::handle(vector, None);
            self.common.leave_critical();
        }
    }
}
