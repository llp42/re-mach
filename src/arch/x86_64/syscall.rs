// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! System calls: their entry point.

use core::arch::global_asm;

use x86_64::VirtAddr;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;

use super::gdt;
use super::per_cpu::{KERNEL_STACK, PRIVILEGE_STACK, USER_STACK};

// SYSCALL enters with interrupts off, rip in rcx and rflags in r11: the
// user registers go into the thread's PCB as a trap from user mode saves
// them, then the kernel stack is taken and interrupts turned on; it stops
// where the call is dispatched.
global_asm!(
    ".pushsection .text, \"ax\"",
    ".globl syscall_entry",
    "syscall_entry:",
    "swapgs",
    "movq %rsp, %gs:{user_stack}",
    "movq %gs:{privilege_stack}, %rsp",
    "pushq ${user_data}",
    "pushq %gs:{user_stack}",
    "pushq %r11",
    "pushq ${user_code}",
    "pushq %rcx",
    // No vector or error code.
    "subq $16, %rsp",
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
    "movq %gs:{kernel_stack}, %rsp",
    "sti",
    // The dispatch comes later.
    "1:",
    "hlt",
    "jmp 1b",
    ".popsection",
    user_stack = const USER_STACK,
    privilege_stack = const PRIVILEGE_STACK,
    user_data = const gdt::USER_DATA.0,
    user_code = const gdt::USER_CODE.0,
    kernel_stack = const KERNEL_STACK,
    options(att_syntax),
);

unsafe extern "C" {
    safe static syscall_entry: u8;
}

/// Turns SYSCALL on, entering at `syscall_entry`, and points SYSRET at the
/// user segments.
///
/// # Safety
///
/// Runs once, on the boot CPU, after `gdt::store_user_segments`.
pub unsafe fn init() {
    // The flags the kernel must not start with.
    SFMask::write(
        RFlags::INTERRUPT_FLAG
            | RFlags::TRAP_FLAG
            | RFlags::DIRECTION_FLAG
            | RFlags::NESTED_TASK
            | RFlags::ALIGNMENT_CHECK
            | RFlags::IOPL_HIGH
            | RFlags::IOPL_LOW
            | RFlags::CARRY_FLAG,
    );
    LStar::write(VirtAddr::from_ptr(&raw const syscall_entry));
    Star::write(
        gdt::USER_CODE,
        gdt::USER_DATA,
        gdt::KERNEL_CODE,
        gdt::KERNEL_DATA,
    )
    .expect("STAR selectors");
    // SAFETY: SYSCALL now enters a kernel entry, with those flags clear.
    unsafe { Efer::update(|flags| flags.insert(EferFlags::SYSTEM_CALL_EXTENSIONS)) };
}
