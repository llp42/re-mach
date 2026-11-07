// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! System calls: their entry point.

use core::arch::global_asm;

use x86_64::VirtAddr;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;

use super::per_cpu::{KERNEL_STACK, PRIVILEGE_STACK, USER_STACK};
use super::trap::TrapFrame;
use super::{Pcb, copy, gdt};
use crate::syscall::MACH_TRAPS;
use crate::thread::MachineThread as _;

// SYSCALL enters with interrupts off, rip in rcx and rflags in r11: the
// user registers go into the thread's PCB as a trap from user mode saves
// them, then the kernel stack is taken, interrupts turned on, and
// `system_call` runs the call.
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
    "movq %rsp, %rdi",
    "movq %gs:{kernel_stack}, %rsp",
    "sti",
    "call {system_call}",
    "ud2",
    ".popsection",
    user_stack = const USER_STACK,
    privilege_stack = const PRIVILEGE_STACK,
    user_data = const gdt::USER_DATA.0,
    user_code = const gdt::USER_CODE.0,
    kernel_stack = const KERNEL_STACK,
    system_call = sym system_call,
    options(att_syntax),
);

unsafe extern "C" {
    safe static syscall_entry: u8;
}

/// Runs the system call `frame` holds: the Mach trap its eax names,
/// negated, with its register arguments, and those past the sixth from the
/// user stack; the result goes to its rax, and the thread returns to user
/// mode.
///
/// # Safety
///
/// `frame` is the current thread's user registers in its PCB.
///
/// # Panics
///
/// On a number outside the table or a stack argument that can't be copied
/// in, which raise exceptions, or a trap not written yet: not yet.
unsafe extern "sysv64" fn system_call(frame: *mut TrapFrame) -> ! {
    {
        // SAFETY: guaranteed by the caller; only the thread touches its
        // user registers while it runs in the kernel.
        let frame = unsafe { &mut *frame };
        let eax = i32::from_le_bytes(frame.rax.to_le_bytes()[..4].try_into().unwrap());
        let trap = usize::try_from(-i64::from(eax))
            .ok()
            .and_then(|index| MACH_TRAPS.get(index))
            .unwrap_or_else(|| panic!("_syscall64_range: not yet"));
        let mut arguments = [
            frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9, 0,
        ]
        .map(|argument| usize::try_from(argument).unwrap());
        // Past the sixth, from the user stack, above the return address.
        let stack = usize::try_from(frame.interrupted.stack_pointer.as_u64())
            .unwrap()
            .wrapping_add(8);
        let count = trap.argument_count().max(6);
        for (index, argument) in arguments[6..count].iter_mut().enumerate() {
            let mut bytes = [0; 8];
            assert!(
                copy::copy_in(stack.wrapping_add(8 * index), &mut bytes).is_ok(),
                "_syscall64_addr_push: not yet"
            );
            *argument = usize::from_le_bytes(bytes);
        }
        frame.rax = u64::from(trap.call(&arguments));
    }
    Pcb::return_to_user()
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
