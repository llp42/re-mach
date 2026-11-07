// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data, reached through the GS base: the machine's part.

use core::arch::asm;
use core::cell::Cell;
use core::mem::{self, MaybeUninit, offset_of};
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, AtomicUsize, Ordering};

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
use crate::thread::Thread;

/// One CPU's machine-dependent data, holding its machine-independent part;
/// the GS base points at the current CPU's. Each starts its own cache line.
#[repr(align(64))]
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
    /// Whether any interrupt is held back, so leaving a section needs one
    /// load to know there is nothing to run.
    interrupts_held: AtomicBool,
    /// Where its thread's kernel stack starts, which entries from user mode
    /// switch to.
    kernel_stack: AtomicUsize,
    /// The user stack pointer of a system call, while its entry saves it.
    user_stack: AtomicUsize,
    /// The thread whose floating-point state its FPU holds, if any.
    fpu_owner: AtomicPtr<Thread>,
}

/// Where the current CPU's kernel stack is, from the GS base.
pub(super) const KERNEL_STACK: usize = offset_of!(PerCpu, kernel_stack);

/// Where the current CPU keeps a system call's user stack pointer, from the
/// GS base.
pub(super) const USER_STACK: usize = offset_of!(PerCpu, user_stack);

/// Where the current CPU's TSS RSP0 is, from the GS base: past its thread's
/// user registers.
pub(super) const PRIVILEGE_STACK: usize =
    offset_of!(PerCpu, tss) + offset_of!(TaskStateSegment, privilege_stack_table);

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
            interrupts_held: AtomicBool::new(false),
            kernel_stack: AtomicUsize::new(0),
            user_stack: AtomicUsize::new(0),
            fpu_owner: AtomicPtr::new(ptr::null_mut()),
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

    /// Its local APIC ID.
    ///
    /// # Panics
    ///
    /// Before `store_apic_id` on its CPU.
    pub fn apic_id(&self) -> u8 {
        *self.apic_id.get().expect("per_cpu: no APIC ID yet")
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

    /// Sets where its thread's kernel stack starts, as it switches to it.
    pub fn set_kernel_stack(&self, top: usize) {
        self.kernel_stack.store(top, Ordering::Relaxed);
    }

    /// The thread whose floating-point state its FPU holds; null for none.
    pub fn fpu_owner(&self) -> *const Thread {
        self.fpu_owner.load(Ordering::Relaxed)
    }

    /// Gives its FPU to `thread`, on its CPU.
    pub fn set_fpu_owner(&self, thread: &'static Thread) {
        self.fpu_owner
            .store(ptr::from_ref(thread).cast_mut(), Ordering::Relaxed);
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
        self.interrupts_held.store(true, Ordering::Relaxed);
        true
    }

    /// Whether this CPU holds any interrupt back; with interrupts off.
    pub fn holds_interrupts(&self) -> bool {
        self.interrupts_held.load(Ordering::Relaxed)
    }

    /// Takes this CPU's lowest pending vector, if any, noting when none is
    /// left; with interrupts off.
    fn take_pending(&self) -> Option<usize> {
        let vector = self
            .pending_interrupts
            .iter()
            .enumerate()
            .find_map(|(index, word)| {
                let bits = word.load(Ordering::Relaxed);
                (bits != 0).then(|| {
                    let bit = bits.trailing_zeros();
                    word.store(bits & !(1 << bit), Ordering::Relaxed);
                    index * 64 + usize::try_from(bit).unwrap()
                })
            });
        if vector.is_none() {
            self.interrupts_held.store(false, Ordering::Relaxed);
        }
        vector
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

    fn of(cpu: usize) -> &'static Self {
        &PER_CPU[cpu]
    }

    fn common(&self) -> &smp::PerCpu {
        &self.common
    }

    fn run_pending_interrupts(&self) {
        // The nesting is 0 by now, so an interrupt arriving after this load
        // is handled at once, not held.
        if !self.interrupts_held.load(Ordering::Relaxed) {
            return;
        }
        // Taking a vector and entering the section in one window keeps a
        // newer interrupt from running before an older one held back.
        while let Some(vector) = interrupts::without_interrupts(|| {
            let vector = self.take_pending();
            if vector.is_some() {
                self.common.enter_critical();
            }
            vector
        }) {
            let interrupt = self.common.enter_interrupt();
            interrupt::handle(vector, None);
            drop(interrupt);
            self.common.leave_critical();
        }
    }
}
