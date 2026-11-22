// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::arch::asm;
use core::ptr;
use core::sync::atomic::Ordering;

use x86_64::registers::control::{Cr0, Cr0Flags};

use super::save::{AREA_OFFSET, FEATURES, FXSAVE_SIZE, SAVE, STATE_SIZE, Save};
use super::state::{DEFAULT_STATE, STATE_CACHE};
use crate::arch::x86_64::MachineState;
use crate::arch::x86_64::per_cpu::PerCpu;
use crate::smp::{self, MachinePerCpu as _};
use crate::sync::CriticalSection;
use crate::thread::thread::Thread;

/// Gives `state` the current thread's x87 control word to start with, if
/// that thread has a floating-point state: from the FPU if it holds that
/// state, else from its buffer.
///
/// # Panics
///
/// If no thread runs on the CPU.
pub fn inherit(state: &MachineState) {
    let _section = CriticalSection::enter();
    let thread = smp::current()
        .active_thread()
        .expect("fpinherit: no current thread");
    let saved = thread.machine_state().fpu.load(Ordering::Relaxed);
    if saved.is_null() {
        return;
    }
    #[expect(
        clippy::cast_ptr_alignment,
        reason = "a saved state's area is 64-byte aligned"
    )]
    let control = if ptr::eq(PerCpu::current().fpu_owner(), thread) {
        let mut control = 0_u16;
        // SAFETY: the FPU holds this thread's state, so TS is clear, and the
        // critical section keeps it there.
        unsafe {
            asm!("fnstcw ({0})", in(reg) ptr::from_mut(&mut control), options(att_syntax, nostack));
        }
        control
    } else {
        // SAFETY: the current thread's saved state, which only a switch from
        // it writes; its area, 64-byte aligned, starts with the control word,
        // and with XSAVE its header, holding XSTATE_BV, follows FXSAVE's area.
        unsafe {
            let area = saved.add(AREA_OFFSET);
            // XSAVEOPT may leave an x87 state in its initial configuration
            // unwritten, with XSTATE_BV's bit 0 clear: the default then.
            let initial = matches!(SAVE.get(), Some(Save::Xsave | Save::XsaveOptimized))
                && area.add(FXSAVE_SIZE).cast::<u64>().read() & 1 == 0;
            if initial {
                0
            } else {
                area.cast::<u16>().read()
            }
        }
    };
    state.init_control.store(control, Ordering::Relaxed);
}

/// Gives the current thread the FPU at a use that trapped, loaded with its
/// saved state; at its first use, a buffer holding the default state, and
/// the FPU loaded with that and the control word it inherited, if any.
///
/// # Panics
///
/// If the FPU holds a thread's state, or no buffer can be allocated.
pub fn no_extension_fault() {
    let thread = smp::current()
        .active_thread()
        .expect("fpnoextflt: no current thread");
    let saved = thread.machine_state().fpu.load(Ordering::Relaxed);
    let (source, control) = if saved.is_null() {
        let size = AREA_OFFSET + *STATE_SIZE.get().expect("fpu: not set up");
        let state = STATE_CACHE
            .get()
            .expect("fpu: module not set up")
            .allocate()
            .unwrap_or_else(|error| panic!("fp_load: {error:?}"));
        let default = DEFAULT_STATE.get().expect("fpu: module not set up").0;
        // SAFETY: both buffers hold `size` bytes; the new one is the
        // thread's alone.
        unsafe { state.copy_from_nonoverlapping(default, size) };
        thread
            .machine_state()
            .fpu
            .store(state.as_ptr(), Ordering::Relaxed);
        let control = thread.machine_state().init_control.load(Ordering::Relaxed);
        (default.as_ptr(), control)
    } else {
        (saved, 0)
    };
    let _section = CriticalSection::enter();
    let cpu = PerCpu::current();
    assert!(
        cpu.fpu_owner().is_null(),
        "fpnoextflt: the FPU holds a thread's state"
    );
    let features = FEATURES.get().copied().unwrap_or(0);
    // SAFETY: the FPU is this thread's from here; the source holds a state
    // a save left, past its header, 64-byte aligned.
    unsafe {
        let area = source.add(AREA_OFFSET);
        asm!("clts", options(att_syntax, nostack));
        // The mask in EDX:EAX: the states XCR0 enables.
        match SAVE.get().copied() {
            Some(Save::Fxsave) | None => {
                asm!("fxrstor64 ({0})", in(reg) area, options(att_syntax, nostack));
            }
            Some(Save::Xsave | Save::XsaveOptimized) => {
                asm!("xrstor64 ({0})", in(reg) area, in("rax") features, in("rdx") features >> 32, options(att_syntax, nostack));
            }
        }
        // A first state's inherited control word, as fpinit.
        if control != 0 {
            asm!("fldcw ({0})", in(reg) ptr::from_ref(&control), options(att_syntax, nostack));
        }
    }
    cpu.set_fpu_owner(thread);
}

/// Saves `thread`'s state to its buffer if the current CPU's FPU holds it,
/// then sets TS so the next use traps and leaves the CPU with no FPU owner;
/// inside a critical section.
pub fn save_context(thread: &Thread) {
    let cpu = PerCpu::current();
    if !ptr::eq(cpu.fpu_owner(), thread) {
        return;
    }
    let state = thread.machine_state().fpu.load(Ordering::Relaxed);
    let features = FEATURES.get().copied().unwrap_or(0);
    // SAFETY: the FPU's owner has a buffer, 64-byte aligned, and TS is
    // clear while it owns the FPU.
    unsafe {
        let area = state.add(AREA_OFFSET);
        // The mask in EDX:EAX: the states XCR0 enables.
        match SAVE.get().copied() {
            Some(Save::Fxsave) | None => {
                asm!("fxsave64 ({0})", in(reg) area, options(att_syntax, nostack));
            }
            Some(Save::Xsave) => {
                asm!("xsave64 ({0})", in(reg) area, in("rax") features, in("rdx") features >> 32, options(att_syntax, nostack));
            }
            Some(Save::XsaveOptimized) => {
                asm!("xsaveopt64 ({0})", in(reg) area, in("rax") features, in("rdx") features >> 32, options(att_syntax, nostack));
            }
        }
        Cr0::update(|flags| flags.insert(Cr0Flags::TASK_SWITCHED));
    }
    cpu.clear_fpu_owner();
}
