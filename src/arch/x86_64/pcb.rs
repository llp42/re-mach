// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads' machine state: their process control blocks.

use core::arch::global_asm;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::fpu;
use super::trap::TrapFrame;
use crate::kernel::Error;
use crate::kernel::slab::{Cache, CacheBox};
use crate::kernel::thread::{self, MachineThread};
use crate::vm::kernel::KernelStack;

/// A thread's machine state: its user registers, and where its kernel
/// registers are saved while it does not run.
#[repr(C, align(16))]
pub struct Pcb {
    /// Its user registers, laid out as a trap from user mode saves them.
    user: TrapFrame,
    /// Its kernel stack pointer while it does not run: the callee-saved
    /// registers a switch pushed, then where it returns.
    stack_pointer: AtomicUsize,
}

// A new thread's first return: interrupts on, then `begin`, then its
// function, which is in rbx and never returns.
global_asm!(
    ".pushsection .text, \"ax\"",
    ".globl thread_trampoline",
    "thread_trampoline:",
    "sti",
    "xorl %ebp, %ebp",
    "call {begin}",
    "call *%rbx",
    "ud2",
    ".popsection",
    begin = sym thread::begin,
    options(att_syntax),
);

unsafe extern "C" {
    /// Where a new thread's first switch returns.
    safe fn thread_trampoline();
}

/// How many callee-saved registers a switch pushes: r15, r14, r13, r12,
/// rbx and rbp, from the lowest address up.
const SAVED_REGISTERS: usize = 6;

/// Where rbx is among them.
const SAVED_RBX: usize = 4;

/// The cache process control blocks come from.
static CACHE: Cache<Pcb> = Cache::new("pcb");

impl MachineThread for Pcb {
    fn module_init() {
        CACHE.register();
        fpu::module_init();
    }

    fn new() -> Result<CacheBox<Self>, Error> {
        CACHE.allocate(Self {
            user: TrapFrame::user(),
            stack_pointer: AtomicUsize::new(0),
        })
    }

    fn attach_stack(&self, stack: &KernelStack, start: fn() -> !) {
        // 16 bytes are left at the top, so the trampoline runs with its
        // stack 16-byte aligned, as calls need.
        let frame =
            ptr::with_exposed_provenance_mut::<usize>(stack.top() - 16 - 8 * (SAVED_REGISTERS + 1));
        // SAFETY: the stack is mapped and the thread's alone, and it does
        // not run yet.
        unsafe {
            frame.write_bytes(0, SAVED_REGISTERS);
            frame.add(SAVED_RBX).cast::<fn() -> !>().write(start);
            frame
                .add(SAVED_REGISTERS)
                .cast::<extern "C" fn()>()
                .write(thread_trampoline);
        }
        self.stack_pointer.store(frame.addr(), Ordering::Release);
    }
}
