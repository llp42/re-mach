// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Traps: their entry points and the kernel's handler.

use core::arch::global_asm;
use core::fmt::{self, Write};

use x86_64::registers::control::Cr2;
use x86_64::structures::idt::InterruptStackFrameValue;

use super::serial::Serial;

// Each entry pushes a zero error code, unless the CPU pushed one, and its
// vector; the common path saves the registers as a `TrapFrame` and calls
// `kernel_trap`.
global_asm!(
    ".pushsection .text, \"ax\"",
    ".macro entry name, vector",
    ".globl \\name",
    "\\name:",
    "pushq $0",
    "pushq $\\vector",
    "jmp .Ltrap_common",
    ".endm",
    ".macro entry_with_error_code name, vector",
    ".globl \\name",
    "\\name:",
    "pushq $\\vector",
    "jmp .Ltrap_common",
    ".endm",
    "entry divide_error_entry, 0",
    "entry debug_entry, 1",
    "entry non_maskable_interrupt_entry, 2",
    "entry breakpoint_entry, 3",
    "entry invalid_opcode_entry, 6",
    "entry device_not_available_entry, 7",
    "entry_with_error_code double_fault_entry, 8",
    "entry_with_error_code invalid_tss_entry, 10",
    "entry_with_error_code segment_not_present_entry, 11",
    "entry_with_error_code stack_segment_fault_entry, 12",
    "entry_with_error_code general_protection_fault_entry, 13",
    "entry_with_error_code page_fault_entry, 14",
    "entry x87_floating_point_entry, 16",
    "entry_with_error_code alignment_check_entry, 17",
    "entry machine_check_entry, 18",
    "entry simd_floating_point_entry, 19",
    "entry virtualization_entry, 20",
    "entry_with_error_code cp_protection_exception_entry, 21",
    "entry hv_injection_exception_entry, 28",
    "entry_with_error_code vmm_communication_exception_entry, 29",
    "entry_with_error_code security_exception_entry, 30",
    ".Ltrap_common:",
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
    // TODO: swap the GS base on traps from user mode.
    "cld",
    "movq %rsp, %rdi",
    "call {kernel_trap}",
    "ud2",
    ".popsection",
    kernel_trap = sym kernel_trap,
    options(att_syntax),
);

// The entries, named after the IDT fields they fill.
unsafe extern "C" {
    pub safe static divide_error_entry: u8;
    pub safe static debug_entry: u8;
    pub safe static non_maskable_interrupt_entry: u8;
    pub safe static breakpoint_entry: u8;
    pub safe static invalid_opcode_entry: u8;
    pub safe static device_not_available_entry: u8;
    pub safe static double_fault_entry: u8;
    pub safe static invalid_tss_entry: u8;
    pub safe static segment_not_present_entry: u8;
    pub safe static stack_segment_fault_entry: u8;
    pub safe static general_protection_fault_entry: u8;
    pub safe static page_fault_entry: u8;
    pub safe static x87_floating_point_entry: u8;
    pub safe static alignment_check_entry: u8;
    pub safe static machine_check_entry: u8;
    pub safe static simd_floating_point_entry: u8;
    pub safe static virtualization_entry: u8;
    pub safe static cp_protection_exception_entry: u8;
    pub safe static hv_injection_exception_entry: u8;
    pub safe static vmm_communication_exception_entry: u8;
    pub safe static security_exception_entry: u8;
}

/// Registers saved on a trap, from the stack top down.
#[repr(C)]
struct TrapFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    vector: u64,
    error_code: u64,
    /// Pushed by the CPU.
    interrupted: InterruptStackFrameValue,
}

impl fmt::Display for TrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let interrupted = &self.interrupted;
        write!(
            f,
            "rax {:#018x} rbx {:#018x} rcx {:#018x} rdx {:#018x}\r\n\
             rsi {:#018x} rdi {:#018x} rbp {:#018x} rsp {:#018x}\r\n\
             r8  {:#018x} r9  {:#018x} r10 {:#018x} r11 {:#018x}\r\n\
             r12 {:#018x} r13 {:#018x} r14 {:#018x} r15 {:#018x}\r\n\
             rflags {:#018x} cs {:#06x} ss {:#06x}\r\n",
            self.rax,
            self.rbx,
            self.rcx,
            self.rdx,
            self.rsi,
            self.rdi,
            self.rbp,
            interrupted.stack_pointer.as_u64(),
            self.r8,
            self.r9,
            self.r10,
            self.r11,
            self.r12,
            self.r13,
            self.r14,
            self.r15,
            interrupted.cpu_flags.bits(),
            interrupted.code_segment.0,
            interrupted.stack_segment.0,
        )
    }
}

/// Reports a kernel trap and panics; none is recoverable yet.
extern "sysv64" fn kernel_trap(frame: &TrapFrame) -> ! {
    let cr2 = Cr2::read_raw();
    let _ = write!(
        Serial,
        "Kernel trap {}, rip {:#x}, code {:#x}, cr2 {cr2:#x}\r\n{frame}",
        frame.vector,
        frame.interrupted.instruction_pointer.as_u64(),
        frame.error_code,
    );
    panic!("trap")
}
