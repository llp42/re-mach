// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! System calls: their entry point.

use core::arch::global_asm;

use x86_64::VirtAddr;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;

use super::memory_layout::MAX_USER_ADDRESS;
use super::per_cpu::{KERNEL_STACK, PRIVILEGE_STACK, USER_STACK};
use super::trap::{TrapFrame, raise_page_fault};
use super::{MachineState, copy, gdt};
use crate::exception::{self, Exception};
use crate::syscall::MACH_TRAPS;
use crate::thread::machine::MachineThread as _;
use crate::vm::Protection;

/// `EXC_I386_INVOP`: the code of a bad instruction's exception.
const INVALID_OPERATION: i32 = 1;

// SYSCALL enters with interrupts off, rip in rcx and rflags in r11: the
// user registers go into the thread's machine state as a trap from user
// mode saves them, then the kernel stack is taken, interrupts turned on,
// and `system_call` runs the call.
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
/// user stack; the result, if any, goes to its rax, and the thread returns
/// to user mode. A number outside the table raises a bad instruction's
/// exception, and a stack argument that can't be copied in a page fault's.
///
/// # Safety
///
/// `frame` is the current thread's user registers in its machine state.
///
/// # Panics
///
/// On a trap not written yet: not yet. As `exception::raise` and
/// `raise_page_fault`.
unsafe extern "sysv64" fn system_call(frame: *mut TrapFrame) -> ! {
    // SAFETY: guaranteed by the caller.
    let rax = unsafe { (*frame).rax };
    let eax = i32::from_le_bytes(rax.to_le_bytes()[..4].try_into().unwrap());
    if let Some(trap) = usize::try_from(-i64::from(eax))
        .ok()
        .and_then(|index| MACH_TRAPS.get(index))
    {
        // SAFETY: guaranteed by the caller; only the thread touches its
        // user registers while it runs in the kernel. The borrow ends before
        // the trap, which may set them.
        let registers = unsafe { &*frame };
        let mut arguments = [
            registers.rdi,
            registers.rsi,
            registers.rdx,
            registers.r10,
            registers.r8,
            registers.r9,
            0,
        ]
        .map(|argument| usize::try_from(argument).unwrap());
        // Past the sixth, from the user stack, above the return address.
        let stack = usize::try_from(registers.interrupted.stack_pointer.as_u64())
            .unwrap()
            .wrapping_add(8);
        let count = trap.argument_count().max(6);
        // As _syscall64_args_stack: their end checked by hand, then each
        // from the last to the first; the first that fails is the fault's.
        let end = stack.wrapping_add(8 * (count - 6));
        let fault = if count > 6 && end >= MAX_USER_ADDRESS {
            Some(end)
        } else {
            let mut fault = None;
            for (index, argument) in arguments[6..count].iter_mut().enumerate().rev() {
                let address = stack.wrapping_add(8 * index);
                let mut bytes = [0; 8];
                if copy::copy_in(address, &mut bytes).is_err() {
                    fault = Some(address);
                    break;
                }
                *argument = usize::from_le_bytes(bytes);
            }
            fault
        };
        match fault {
            None => {
                if let Some(result) = trap.call(&arguments) {
                    // SAFETY: as above; no borrow of them is alive.
                    unsafe { (*frame).rax = u64::from(result) };
                }
            }
            // _syscall64_addr_push: user_trap's page fault, reading; the
            // frame is not used past here, as others may change it while
            // the thread waits.
            Some(address) => raise_page_fault(address, Protection::READ),
        }
    } else {
        // As _syscall64_range, then user_trap's invalid opcode: the frame is
        // not borrowed, as others may change it while the thread waits.
        exception::raise(Exception::BadInstruction, INVALID_OPERATION, 0);
    }
    MachineState::return_to_user()
}

/// Turns SYSCALL on, entering at `syscall_entry`, and points SYSRET at the
/// user segments.
///
/// # Safety
///
/// Runs once on each CPU, after `gdt::store_user_segments` with its table.
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
