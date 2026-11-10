// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Interrupts: their entry points and handler.

use core::arch::global_asm;
use core::fmt::Write;
use core::mem::offset_of;

use x86_64::VirtAddr;

use x86_64::PrivilegeLevel;
use x86_64::structures::idt::InterruptStackFrameValue;

use super::per_cpu::{KERNEL_STACK, PerCpu};
use super::serial::Serial;
use super::trap::TrapFrame;
use super::{MachineState, apic, idt};
use crate::clock;
use crate::config::HZ;
use crate::machine::Mode;
use crate::smp::MachinePerCpu as _;
use crate::sync::CriticalSection;
use crate::thread::MachineThread as _;

// Each entry pushes a zero error code and its vector, and adds its address
// to `entries`; the common path saves the registers as a `TrapFrame`. An
// interrupt from user mode, saved in the thread's machine state, swaps the
// GS base and calls `user_interrupt` on the kernel stack; one from kernel
// mode calls `interrupt` and returns.
global_asm!(
    ".pushsection .text, \"ax\"",
    ".pushsection .rodata, \"a\"",
    ".balign 8",
    ".globl entries",
    "entries:",
    ".popsection",
    ".set .Lvector, 32",
    ".rept 224",
    ".pushsection .rodata, \"a\"",
    ".quad 1f",
    ".popsection",
    "1:",
    "pushq $0",
    "pushq $.Lvector",
    "jmp .Linterrupt_common",
    ".set .Lvector, .Lvector + 1",
    ".endr",
    ".Linterrupt_common:",
    // SMAP closes, from either mode.
    "nopl (%rax)",
    ".pushsection smap_close, \"a\"",
    ".balign 8",
    ".quad .Linterrupt_common",
    ".popsection",
    "pushq %rax",
    "pushq %rbx",
    "pushq %rcx",
    "pushq %rdx",
    "pushq %rsi",
    "pushq %rdi",
    "pushq %rbp",
    "pushq %r8",
    "pushq %r9",
    "pushq %r10",
    "pushq %r11",
    "pushq %r12",
    "pushq %r13",
    "pushq %r14",
    "pushq %r15",
    "testb $3, {code_segment}(%rsp)",
    "jz .Linterrupt_from_kernel",
    "swapgs",
    "cld",
    "movq %rsp, %rdi",
    "movq %gs:{kernel_stack}, %rsp",
    "call {user_interrupt}",
    "ud2",
    ".Linterrupt_from_kernel:",
    "cld",
    "movq %rsp, %rdi",
    "call {interrupt}",
    "popq %r15",
    "popq %r14",
    "popq %r13",
    "popq %r12",
    "popq %r11",
    "popq %r10",
    "popq %r9",
    "popq %r8",
    "popq %rbp",
    "popq %rdi",
    "popq %rsi",
    "popq %rdx",
    "popq %rcx",
    "popq %rbx",
    "popq %rax",
    "addq $16, %rsp",
    "iretq",
    ".popsection",
    code_segment = const offset_of!(TrapFrame, interrupted)
        + offset_of!(InterruptStackFrameValue, code_segment),
    kernel_stack = const KERNEL_STACK,
    user_interrupt = sym user_interrupt,
    interrupt = sym interrupt,
    options(att_syntax),
);

unsafe extern "C" {
    /// The entries of vectors 0x20 to 0xff, in order.
    pub safe static entries: [u64; 224];
}

/// Takes an interrupt from user mode, then returns the thread to user mode,
/// taking its ASTs first.
extern "sysv64" fn user_interrupt(frame: &TrapFrame) -> ! {
    interrupt(frame);
    MachineState::return_to_user()
}

/// Holds the interrupt back inside a critical section, and handles it in
/// one otherwise.
extern "sysv64" fn interrupt(frame: &TrapFrame) {
    let cpu = PerCpu::current();
    let _interrupt = cpu.common().enter_interrupt();
    // The local APIC's spurious vector, which takes no EOI.
    if frame.vector == 0xff {
        return;
    }
    // The timer's EOI comes first, so a tick held back still lets the next
    // one in; held ticks run once.
    if frame.vector == apic::TIMER_VECTOR {
        apic::end_of_interrupt();
    }
    if cpu.hold(frame.vector) {
        return;
    }
    let _section = CriticalSection::enter();
    handle(frame.vector, Some(frame));
}

/// Runs `vector`'s handler, in a critical section, with the state it
/// interrupted unless it was held back; the timer's is the clock's, and
/// no other source is unmasked yet.
pub fn handle(vector: usize, frame: Option<&TrapFrame>) {
    if vector == apic::TIMER_VECTOR {
        // A tick held back interrupted the kernel.
        let mode = match frame {
            Some(frame) if frame.interrupted.code_segment.rpl() != PrivilegeLevel::Ring0 => {
                Mode::User
            }
            _ => Mode::Kernel,
        };
        clock::interrupt(1_000_000 / HZ, mode);
        return;
    }
    let _ = write!(Serial, "Unexpected interrupt {vector}");
    let _ = frame.map_or_else(
        || write!(Serial, ", held back\r\n"),
        |frame| {
            write!(
                Serial,
                ", rip {:#x}\r\n{frame}",
                frame.interrupted.instruction_pointer.as_u64(),
            )
        },
    );
    panic!("interrupt")
}

/// Points the boot CPU's interrupt vectors at their entries.
///
/// # Safety
///
/// Runs once, on the boot CPU, after `idt::init`, with interrupts off.
pub unsafe fn init() {
    let mut vectors = idt::interrupts();
    for (vector, &address) in vectors.iter_mut().zip(&entries) {
        // SAFETY: the address is the vector's entry, built for the frame
        // the CPU pushes on an interrupt.
        unsafe { vector.set_handler_addr(VirtAddr::new(address)) };
    }
}
