// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Task state segments, one per CPU, and their IST stacks.

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicU32, Ordering};

use super::gdt::{KERNEL_PRIVILEGE, TSS_INDEX, selector};
use super::instructions;
use crate::smp::PerCpu;

/// IST slot for NMI and #DF (dfly's `dblstack`).
pub const IST_DOUBLE_FAULT: u8 = 1;
/// IST slot for #DB (dfly's `dbgstack`).
pub const IST_DEBUG: u8 = 2;
/// IST slot for #MC (Linux's `ESTACK_MCE`).
pub const IST_MACHINE_CHECK: u8 = 3;

/// Bytes per IST stack: two pages, as dfly and Linux use.
const STACK_SIZE: usize = 8192;

/// A stack only the CPU uses, through an IST slot.
#[repr(C, align(16))]
pub struct Stack(UnsafeCell<[u8; STACK_SIZE]>);

// SAFETY: Rust code never touches the bytes; only the CPU writes them, as a stack.
unsafe impl Sync for Stack {}

impl Stack {
    pub const fn new() -> Self {
        Self(UnsafeCell::new([0; STACK_SIZE]))
    }

    /// The address past the end, where the CPU starts pushing.
    fn top(&self) -> u64 {
        u64::try_from(self.0.get().expose_provenance() + STACK_SIZE).unwrap()
    }
}

/// A 64-bit TSS as the 32-bit words of its Intel SDM layout; atomic, so it
/// can be set through `&self`.
#[repr(C)]
pub struct Tss([AtomicU32; 26]);

impl Tss {
    /// All zero, with the I/O map base at the end (104): no I/O bitmap.
    pub const fn new() -> Self {
        let mut words = [const { AtomicU32::new(0) }; 26];
        words[25] = AtomicU32::new(104 << 16);
        Self(words)
    }

    /// Sets IST slot `index` (1-7) to `top`.
    ///
    /// # Safety
    ///
    /// `top` is the end of a stack used only by the vectors that name this slot.
    pub unsafe fn set_ist(&self, index: u8, top: u64) {
        let word = 7 + 2 * usize::from(index);
        self.0[word].store(u32::try_from(top & 0xffff_ffff).unwrap(), Ordering::Relaxed);
        self.0[word + 1].store(u32::try_from(top >> 32).unwrap(), Ordering::Relaxed);
    }
}

/// Points `per_cpu`'s IST slots at its stacks, installs its TSS in the GDT and
/// loads it.
pub fn init(per_cpu: &'static PerCpu) {
    let md = &per_cpu.md;
    let tss = &md.tss;
    // SAFETY: each stack belongs to one slot only.
    unsafe {
        tss.set_ist(IST_DOUBLE_FAULT, md.double_fault_stack.top());
        tss.set_ist(IST_DEBUG, md.debug_stack.top());
        tss.set_ist(IST_MACHINE_CHECK, md.machine_check_stack.top());
    }
    md.gdt.set_tss(tss);
    // SAFETY: the GDT holds an available TSS descriptor at `TSS_INDEX`, and
    // the TSS is static.
    unsafe { instructions::load_task_register(selector(TSS_INDEX, KERNEL_PRIVILEGE)) };
}
