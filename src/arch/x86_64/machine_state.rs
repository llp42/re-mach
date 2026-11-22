// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads' machine state: their process control blocks.

use core::arch::{asm, global_asm, naked_asm};
use core::cell::UnsafeCell;
use core::ops::Range;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU16, AtomicU64, AtomicUsize, Ordering};

use x86_64::instructions::interrupts;
use x86_64::registers::model_specific::{FsBase, KernelGsBase};
use x86_64::structures::gdt::SegmentSelector;
use x86_64::{PrivilegeLevel, VirtAddr};

use super::fpu::{self, state::FloatState};
use super::gdt::{USER_CODE, USER_DATA};
use super::memory_layout::MAX_USER_ADDRESS;
use super::per_cpu::PerCpu;
use super::thread_state::{FsGsBase, StateFlavor, UserRegisters};
use super::trap::TrapFrame;
use crate::ast;
use crate::error::Error;
use crate::slab::{Cache, CacheBox};
use crate::smp::{self, MachinePerCpu as _};
use crate::thread::create;
use crate::thread::machine::MachineThread;
use crate::thread::thread::Thread;
use crate::vm::kernel::KernelStack;

/// A thread's machine state: its user registers, and where its kernel
/// registers are saved while it does not run.
#[repr(C, align(16))]
pub struct MachineState {
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
    pub(super) stack_top: AtomicUsize,
    /// Its floating-point state's buffer, once it first uses the FPU.
    pub(super) fpu: AtomicPtr<u8>,
    /// The x87 control word its first FPU state takes, its creator's; 0
    /// for the default's.
    pub(super) init_control: AtomicU16,
    /// Its user FS base.
    fs_base: AtomicU64,
    /// Its user GS base, which `KERNEL_GS_BASE` holds while it runs in the
    /// kernel.
    gs_base: AtomicU64,
}

// SAFETY: its thread, or the CPU as that thread traps, writes its user
// registers; others read or write them only while it is stopped.
unsafe impl Sync for MachineState {}

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
    begin = sym create::begin,
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
static CACHE: Cache<MachineState> = Cache::new("pcb");

impl MachineState {
    /// Writes its user FS and GS bases to the current CPU's MSRs.
    fn load_user_bases(&self) {
        // SAFETY: the kernel never uses FS.
        unsafe { FsBase::write(VirtAddr::new(self.fs_base.load(Ordering::Relaxed))) };
        // SAFETY: user GS base, for `swapgs`.
        unsafe { KernelGsBase::write(VirtAddr::new(self.gs_base.load(Ordering::Relaxed))) };
    }
}

impl MachineThread for MachineState {
    fn module_init() {
        CACHE.register();
        fpu::init::module_init();
    }

    fn new() -> Result<CacheBox<Self>, Error> {
        CACHE.allocate(Self {
            user: UnsafeCell::new(TrapFrame::user()),
            stack_pointer: AtomicUsize::new(0),
            onfault: AtomicUsize::new(0),
            onfault_stack: AtomicUsize::new(0),
            stack_top: AtomicUsize::new(0),
            fpu: AtomicPtr::new(ptr::null_mut()),
            init_control: AtomicU16::new(0),
            fs_base: AtomicU64::new(0),
            gs_base: AtomicU64::new(0),
        })
    }

    fn inherit(&self) {
        fpu::context::inherit(self);
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
        // A trap from user mode saves the thread's registers in its machine
        // state.
        let user_end = self.user.get().addr() + size_of::<TrapFrame>();
        let cpu = PerCpu::current();
        let tss = cpu.tss();
        let mut updated = tss.get();
        updated.privilege_stack_table[0] = VirtAddr::new(u64::try_from(user_end).unwrap());
        tss.set(updated);
        cpu.set_kernel_stack(self.stack_top.load(Ordering::Relaxed));
        self.load_user_bases();
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
        fpu::context::save_context(current);
        let cpu = PerCpu::current();
        // A trap from user mode saves the thread's registers in its machine
        // state.
        let user_end = next.user.get().addr() + size_of::<TrapFrame>();
        let tss = cpu.tss();
        let mut updated = tss.get();
        updated.privilege_stack_table[0] = VirtAddr::new(u64::try_from(user_end).unwrap());
        tss.set(updated);
        cpu.set_kernel_stack(next.stack_top.load(Ordering::Relaxed));
        next.load_user_bases();
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
        let user = unsafe { &mut *thread.machine_state().user.get() };
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
        // saved or `set_user_registers` or `write_state` set; the kernel
        // stack holds nothing live past here, and interrupts stay off until
        // iretq.
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
                user = in(reg) thread.machine_state().user.get(),
                options(att_syntax, noreturn),
            );
        }
    }

    unsafe fn read_state(&self, flavor: i32, state: &mut [u32]) -> Result<usize, Error> {
        match StateFlavor::try_from(flavor) {
            Ok(StateFlavor::Registers | StateFlavor::RegistersAndSegments) => {
                let Some(words) = state.first_chunk_mut::<{ UserRegisters::COUNT }>() else {
                    return Err(Error::InvalidArgument);
                };
                // SAFETY: guaranteed by the caller: its thread is stopped, so
                // nothing writes its user registers.
                let user = UserRegisters::from(unsafe { &*self.user.get() });
                // SAFETY: `words` holds a `UserRegisters`'s bytes, which its
                // fields, padding included, all set.
                unsafe {
                    ptr::from_mut(words)
                        .cast::<UserRegisters>()
                        .write_unaligned(user);
                }
                Ok(UserRegisters::COUNT)
            }
            Ok(StateFlavor::Float) => {
                let Some(words) = state.first_chunk_mut::<{ FloatState::COUNT }>() else {
                    return Err(Error::InvalidArgument);
                };
                // SAFETY: guaranteed by the caller: its thread is stopped.
                let float = unsafe { fpu::state::read_state(self) };
                // SAFETY: `words` holds a `FloatState`'s bytes, which its
                // fields, padding included, all set.
                unsafe {
                    ptr::from_mut(words)
                        .cast::<FloatState>()
                        .write_unaligned(float);
                }
                Ok(FloatState::COUNT)
            }
            Ok(StateFlavor::Debug) => panic!("thread_getstatus: i386_DEBUG_STATE, not yet"),
            Ok(StateFlavor::FsGsBase) => {
                panic!("thread_getstatus: i386_FSGS_BASE_STATE, not yet")
            }
            Ok(StateFlavor::ExtendedFloat) => {
                let count = fpu::state::xstate_size() / size_of::<u32>();
                let Some(words) = state.get_mut(..count) else {
                    return Err(Error::InvalidArgument);
                };
                // SAFETY: guaranteed by the caller: its thread is stopped;
                // `words` holds `xstate_size` bytes.
                unsafe { fpu::state::read_extended_state(self, words) };
                Ok(count)
            }
            Err(flavor) => panic!("thread_getstatus: flavor {flavor}, not yet"),
        }
    }

    fn reads_own_state(flavor: i32) -> bool {
        matches!(StateFlavor::try_from(flavor), Ok(StateFlavor::Debug))
    }

    unsafe fn write_state(&self, flavor: i32, state: &[u32]) -> Result<(), Error> {
        match StateFlavor::try_from(flavor) {
            Ok(kind @ (StateFlavor::Registers | StateFlavor::RegistersAndSegments)) => {
                let Some(words) = state.first_chunk::<{ UserRegisters::COUNT }>() else {
                    return Err(Error::InvalidArgument);
                };
                // SAFETY: `words` holds a `UserRegisters`'s bytes, and any
                // bytes make one.
                let mut registers = unsafe {
                    ptr::from_ref(words)
                        .cast::<UserRegisters>()
                        .read_unaligned()
                };
                if matches!(kind, StateFlavor::Registers) {
                    // The user selectors, whatever the state holds.
                    registers.cs = u32::from(USER_CODE.0);
                    registers.ss = u32::from(USER_DATA.0);
                } else {
                    // Only the selectors' low 16 bits count; each must be a
                    // user one, not null.
                    registers.cs &= 0xffff;
                    registers.ss &= 0xffff;
                    let user = |selector: u32| {
                        selector != 0
                            && SegmentSelector(u16::try_from(selector).unwrap()).rpl()
                                == PrivilegeLevel::Ring3
                    };
                    if !(user(registers.cs) && user(registers.ss)) {
                        return Err(Error::InvalidArgument);
                    }
                }
                // SAFETY: guaranteed by the caller: its thread is stopped, so
                // nothing else reads or writes its user registers.
                unsafe { &mut *self.user.get() }.set_registers(&registers);
                Ok(())
            }
            Ok(StateFlavor::Float) => {
                let Some(words) = state.first_chunk::<{ FloatState::COUNT }>() else {
                    return Err(Error::InvalidArgument);
                };
                // SAFETY: `words` holds a `FloatState`'s bytes, and any bytes
                // make one.
                let float = unsafe { ptr::from_ref(words).cast::<FloatState>().read_unaligned() };
                // SAFETY: guaranteed by the caller: its thread is stopped, or
                // is the current thread.
                unsafe { fpu::state::write_state(self, &float) };
                Ok(())
            }
            Ok(StateFlavor::Debug) => panic!("thread_setstatus: i386_DEBUG_STATE, not yet"),
            Ok(StateFlavor::FsGsBase) => {
                let Some(words) = state.first_chunk::<{ FsGsBase::COUNT }>() else {
                    return Err(Error::InvalidArgument);
                };
                // SAFETY: `words` holds a `FsGsBase`'s bytes, and any bytes
                // make one.
                let bases = unsafe { ptr::from_ref(words).cast::<FsGsBase>().read_unaligned() };
                // The MSRs take canonical addresses only.
                if VirtAddr::try_new(bases.fs_base).is_err()
                    || VirtAddr::try_new(bases.gs_base).is_err()
                {
                    return Err(Error::InvalidArgument);
                }
                self.fs_base.store(bases.fs_base, Ordering::Relaxed);
                self.gs_base.store(bases.gs_base, Ordering::Relaxed);
                if smp::current()
                    .active_thread()
                    .is_some_and(|current| ptr::eq(current.machine_state(), self))
                {
                    self.load_user_bases();
                }
                Ok(())
            }
            Ok(StateFlavor::ExtendedFloat) => {
                let count = fpu::state::xstate_size() / size_of::<u32>();
                let Some(words) = state.get(..count) else {
                    return Err(Error::InvalidArgument);
                };
                // SAFETY: guaranteed by the caller: its thread is stopped, or
                // is the current thread; `words` holds `xstate_size` bytes.
                unsafe { fpu::state::write_extended_state(self, words) }
            }
            Err(flavor) => panic!("thread_setstatus: flavor {flavor}, not yet"),
        }
    }

    fn writes_own_state(flavor: i32) -> bool {
        matches!(
            StateFlavor::try_from(flavor),
            Ok(StateFlavor::Debug | StateFlavor::FsGsBase)
        )
    }

    unsafe fn write_own_state(&self, flavor: i32, state: &[u32]) -> Result<(), Error> {
        // As thread_set_syscall_return: KERN_SUCCESS.
        // SAFETY: guaranteed by the caller: nothing borrows them.
        unsafe { (*self.user.get()).rax = 0 };
        // SAFETY: guaranteed by the caller: the current thread in a system
        // call, its user registers not borrowed.
        unsafe { self.write_state(flavor, state) }
    }

    unsafe fn terminate(state: NonNull<Self>) {
        // SAFETY: guaranteed by the caller; its thread's last switch moved
        // the TSS's and the CPU's stack pointers to the next thread.
        let state = unsafe { CacheBox::from_raw(&CACHE, state) };
        if let Some(buffer) = NonNull::new(state.fpu.load(Ordering::Relaxed)) {
            // SAFETY: its thread's buffer; that thread's last switch saved
            // its state there and cleared the CPU's FPU owner.
            unsafe { fpu::state::free(buffer) };
        }
        // No LDT exists to free.
        drop(state);
    }
}
