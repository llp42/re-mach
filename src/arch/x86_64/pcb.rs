// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads' machine state: their process control blocks.

use core::arch::{asm, global_asm, naked_asm};
use core::cell::UnsafeCell;
use core::ops::Range;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use x86_64::VirtAddr;
use x86_64::instructions::interrupts;

use super::fpu;
use super::memory_layout::MAX_USER_ADDRESS;
use super::per_cpu::PerCpu;
use super::trap::TrapFrame;
use crate::ast;
use crate::error::Error;
use crate::slab::{Cache, CacheBox};
use crate::smp::{self, MachinePerCpu as _};
use crate::thread::{self, MachineThread, Thread};
use crate::vm::kernel::{KERNEL_MAP, KernelStack};

/// A thread's machine state: its user registers, and where its kernel
/// registers are saved while it does not run.
#[repr(C, align(16))]
pub struct Pcb {
    /// Its user registers, laid out as a trap from user mode saves them.
    user: UnsafeCell<TrapFrame>,
    /// Its kernel stack pointer while it does not run: the callee-saved
    /// registers a switch pushed, then where it returns.
    stack_pointer: AtomicUsize,
    /// Where a copy resumes after a fault on user memory that can't be
    /// resolved; 0 outside a copy.
    pub(super) onfault: AtomicUsize,
    /// The stack pointer of the copy `onfault` serves.
    pub(super) onfault_stack: AtomicUsize,
    /// Where its kernel stack starts; set as the stack is attached.
    stack_top: AtomicUsize,
    /// Its floating-point state's buffer, once it first uses the FPU.
    pub(super) fpu: AtomicPtr<u8>,
}

// SAFETY: only its thread, or the CPU as that thread traps, writes its user
// registers.
unsafe impl Sync for Pcb {}

/// What a user stack pointer is aligned to, as the System V ABI asks.
const USER_STACK_ALIGNMENT: usize = 16;

// A new thread's first return: interrupts on, then `begin` with the thread
// switched from, in rax, then its function, which is in rbx and never
// returns.
global_asm!(
    ".pushsection .text, \"ax\"",
    ".globl thread_trampoline",
    "thread_trampoline:",
    "sti",
    "xorl %ebp, %ebp",
    "movq %rax, %rdi",
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

/// Pushes the callee-saved registers, stores the stack pointer at
/// `from_stack`, and pops them from `to_stack`, returning there with
/// `from`, the thread switched from.
///
/// # Safety
///
/// `to_stack` holds a frame a switch pushed or `attach_stack` wrote, and
/// the CPU is inside a critical section.
#[unsafe(naked)]
unsafe extern "sysv64" fn switch_stacks(
    from_stack: *mut usize,
    to_stack: usize,
    from: &'static Thread,
) -> &'static Thread {
    naked_asm!(
        "pushq %rbp",
        "pushq %rbx",
        "pushq %r12",
        "pushq %r13",
        "pushq %r14",
        "pushq %r15",
        "movq %rsp, (%rdi)",
        "movq %rsi, %rsp",
        "movq %rdx, %rax",
        "popq %r15",
        "popq %r14",
        "popq %r13",
        "popq %r12",
        "popq %rbx",
        "popq %rbp",
        "ret",
        options(att_syntax),
    );
}

/// The cache process control blocks come from.
static CACHE: Cache<Pcb> = Cache::new("pcb");

impl MachineThread for Pcb {
    fn module_init() {
        CACHE.register();
        fpu::module_init();
    }

    fn new() -> Result<CacheBox<Self>, Error> {
        CACHE.allocate(Self {
            user: UnsafeCell::new(TrapFrame::user()),
            stack_pointer: AtomicUsize::new(0),
            onfault: AtomicUsize::new(0),
            onfault_stack: AtomicUsize::new(0),
            stack_top: AtomicUsize::new(0),
            fpu: AtomicPtr::new(ptr::null_mut()),
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
        self.stack_top.store(stack.top(), Ordering::Relaxed);
    }

    unsafe fn load(&self) -> ! {
        // A trap from user mode saves the thread's registers in its PCB.
        let user_end = self.user.get().addr() + size_of::<TrapFrame>();
        let cpu = PerCpu::current();
        let tss = cpu.tss();
        let mut updated = tss.get();
        updated.privilege_stack_table[0] = VirtAddr::new(u64::try_from(user_end).unwrap());
        tss.set(updated);
        cpu.set_kernel_stack(self.stack_top.load(Ordering::Relaxed));
        // SAFETY: guaranteed by the caller; the frame is the one
        // `attach_stack` wrote.
        unsafe {
            asm!(
                "movq {stack_pointer}, %rsp",
                "popq %r15",
                "popq %r14",
                "popq %r13",
                "popq %r12",
                "popq %rbx",
                "popq %rbp",
                // No thread is switched from.
                "xorl %eax, %eax",
                "ret",
                stack_pointer = in(reg) self.stack_pointer.load(Ordering::Acquire),
                options(att_syntax, noreturn),
            );
        }
    }

    unsafe fn switch(&self, next: &Self, current: &'static Thread) -> &'static Thread {
        let cpu = PerCpu::current();
        // With no FPU save here yet, another user thread must not find the
        // owner's state live; the CPU already names `next` its thread.
        let next_thread = smp::current()
            .active_thread()
            .expect("switch_context: no next thread");
        let owner = cpu.fpu_owner();
        let kernel = KERNEL_MAP
            .get()
            .is_some_and(|map| ptr::eq(map, &raw const *next_thread.task().map));
        assert!(
            owner.is_null() || ptr::eq(owner, next_thread) || kernel,
            "fpu_save_context: not yet"
        );
        // A trap from user mode saves the thread's registers in its PCB.
        let user_end = next.user.get().addr() + size_of::<TrapFrame>();
        let tss = cpu.tss();
        let mut updated = tss.get();
        updated.privilege_stack_table[0] = VirtAddr::new(u64::try_from(user_end).unwrap());
        tss.set(updated);
        cpu.set_kernel_stack(next.stack_top.load(Ordering::Relaxed));
        // SAFETY: guaranteed by the caller; this thread's stack pointer is
        // saved where its next switch back reads it.
        unsafe {
            switch_stacks(
                self.stack_pointer.as_ptr(),
                next.stack_pointer.load(Ordering::Acquire),
                current,
            )
        }
    }

    fn user_stack_low(size: usize) -> usize {
        MAX_USER_ADDRESS - size
    }

    fn set_user_registers(stack: Range<usize>, entry: usize, argument_size: usize) -> usize {
        assert!(
            stack.start.is_multiple_of(USER_STACK_ALIGNMENT)
                && stack.end.is_multiple_of(USER_STACK_ALIGNMENT)
        );
        let arguments = stack.end - argument_size.next_multiple_of(USER_STACK_ALIGNMENT);
        let thread = smp::current()
            .active_thread()
            .expect("set_user_regs: no current thread");
        // SAFETY: the current thread's own registers, written by nothing else
        // while it runs in the kernel.
        let user = unsafe { &mut *thread.pcb().user.get() };
        user.interrupted.stack_pointer = VirtAddr::new(u64::try_from(arguments).unwrap());
        user.interrupted.instruction_pointer = VirtAddr::new(u64::try_from(entry).unwrap());
        arguments
    }

    fn return_to_user() -> ! {
        // Checked with interrupts off, taken with them on.
        loop {
            interrupts::disable();
            if smp::current().asts().is_empty() {
                break;
            }
            interrupts::enable();
            ast::taken();
        }
        let thread = smp::current()
            .active_thread()
            .expect("thread_exception_return: no current thread");
        // SAFETY: the thread's user registers, which a trap from user mode
        // saved or `set_user_registers` set; the kernel stack holds nothing
        // live past here, and interrupts stay off until iretq.
        unsafe {
            asm!(
                "movq {user}, %rsp",
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
                // The vector and error code.
                "addq $16, %rsp",
                "swapgs",
                "iretq",
                user = in(reg) thread.pcb().user.get(),
                options(att_syntax, noreturn),
            );
        }
    }
}
