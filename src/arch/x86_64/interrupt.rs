// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Interrupts: their entry points and handler.

use core::arch::global_asm;
use core::fmt::Write;
use core::mem::offset_of;

use x86_64::VirtAddr;

use super::idt;
use super::per_cpu::PerCpu;
use super::serial::Serial;
use super::trap::TrapFrame;

// Each entry pushes a zero error code and its vector, and adds its address
// to `entries`; the common path saves the registers as a `TrapFrame`, calls
// `interrupt` with the CPU's interrupt nesting raised, and returns.
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
    // TODO: swap the GS base on interrupts from user mode.
    "cld",
    "incl %gs:{nesting}",
    "movq %rsp, %rdi",
    "call {interrupt}",
    "decl %gs:{nesting}",
    // TODO: swap it back on returns to user mode.
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
    interrupt = sym interrupt,
    nesting = const offset_of!(PerCpu, interrupt_nesting),
    options(att_syntax),
);

unsafe extern "C" {
    /// The entries of vectors 0x20 to 0xff, in order.
    pub safe static entries: [u64; 224];
}

/// Panics on any interrupt but the spurious one; no source is unmasked yet.
extern "sysv64" fn interrupt(frame: &TrapFrame) {
    // The local APIC's spurious vector, which takes no EOI.
    if frame.vector == 0xff {
        return;
    }
    let _ = write!(
        Serial,
        "Unexpected interrupt {}, rip {:#x}\r\n{frame}",
        frame.vector,
        frame.interrupted.instruction_pointer.as_u64(),
    );
    panic!("interrupt")
}

/// Points the boot CPU's interrupt vectors at their entries.
///
/// # Safety
///
/// Runs once, on the boot CPU, after `idt::init`, with interrupts off.
pub unsafe fn init() {
    // SAFETY: guaranteed by the caller.
    let vectors = unsafe { idt::interrupts() };
    for (vector, &address) in vectors.iter_mut().zip(&entries) {
        // SAFETY: the address is the vector's entry, built for the frame
        // the CPU pushes on an interrupt.
        unsafe { vector.set_handler_addr(VirtAddr::new(address)) };
    }
}
