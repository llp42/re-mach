// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ops::Range;
use core::ptr::NonNull;

use crate::error::Error;
use crate::slab::CacheBox;
use crate::vm::kernel::KernelStack;

use super::thread::Thread;

/// What threads need of the machine.
pub trait MachineThread: Sized {
    /// Sets up threads' machine state, on the boot CPU.
    fn module_init();
    /// A new thread's machine state, which starts it in user mode.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if none can be allocated.
    fn new() -> Result<CacheBox<Self>, Error>;
    /// Gives its thread, new in the current thread's task, what it inherits
    /// from the current thread.
    ///
    /// # Panics
    ///
    /// If no thread runs on the CPU.
    fn inherit(&self);
    /// Sets `stack` up to start its thread in `start`, which the thread
    /// enters by calling `begin`, once the switch to it is made.
    fn attach_stack(&self, stack: &KernelStack, start: fn() -> !);
    /// Switches the current CPU to its thread for good.
    ///
    /// # Safety
    ///
    /// Its stack is set up, and the CPU is inside a critical section the
    /// thread's count holds.
    unsafe fn load(&self) -> !;
    /// Switches the current CPU from its thread, `current`, to `next`'s;
    /// once its thread runs again, returns the thread switched from then.
    ///
    /// # Safety
    ///
    /// `next`'s stack is set up, and the CPU is inside a critical section
    /// both threads' counts hold.
    unsafe fn switch(&self, next: &Self, current: &'static Thread) -> &'static Thread;
    /// Where a user stack of `size` bytes starts.
    fn user_stack_low(size: usize) -> usize;
    /// Sets the current thread's first user registers: its stack pointer to
    /// room for `argument_size` bytes, rounded up to the stack's alignment,
    /// at the top of `stack`, and its instruction pointer to `entry`; returns
    /// where that room starts.
    ///
    /// # Panics
    ///
    /// If `stack` is not aligned, or no thread runs on the CPU.
    fn set_user_registers(stack: Range<usize>, entry: usize, argument_size: usize) -> usize;
    /// Takes the current CPU's ASTs until none is pending, then returns its
    /// thread to user mode.
    fn return_to_user() -> !;
    /// Writes its thread's user state of `flavor` into `state`, whose
    /// length is the count asked for; gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if `state` is too short for the flavor.
    ///
    /// # Panics
    ///
    /// For a flavor not written here: not yet.
    ///
    /// # Safety
    ///
    /// Its thread is stopped.
    unsafe fn read_state(&self, flavor: i32, state: &mut [u32]) -> Result<usize, Error>;
    /// Whether a thread reads its own state of `flavor` directly, without
    /// being stopped.
    fn reads_own_state(flavor: i32) -> bool;
    /// Sets its thread's user state of `flavor` from `state`, whose length
    /// is the count given.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if `state` is too short for the flavor, or holds
    /// values the flavor refuses.
    ///
    /// # Panics
    ///
    /// For a flavor, or values, not written here: not yet.
    ///
    /// # Safety
    ///
    /// Its thread is stopped, or is the current thread and `flavor` is one
    /// `writes_own_state` names, or is the current thread in a system call
    /// and nothing borrows its user registers.
    unsafe fn write_state(&self, flavor: i32, state: &[u32]) -> Result<(), Error>;
    /// Whether a thread writes its own state of `flavor` directly, without
    /// being stopped.
    fn writes_own_state(flavor: i32) -> bool;
    /// Sets the current thread's own user state of `flavor` from `state`,
    /// as `thread_set_self_state` does: its system call's result is
    /// `KERN_SUCCESS` first, which the state may replace.
    ///
    /// # Errors
    ///
    /// As `write_state`.
    ///
    /// # Panics
    ///
    /// As `write_state`.
    ///
    /// # Safety
    ///
    /// Its thread is the current thread, in a system call, and nothing
    /// borrows its user registers.
    unsafe fn write_own_state(&self, flavor: i32, state: &[u32]) -> Result<(), Error>;
    /// Frees `state`, with its floating-point state.
    ///
    /// # Safety
    ///
    /// `state` is a box from `new`, given up, and its thread was switched
    /// from for the last time: no CPU uses `state` or holds its
    /// floating-point state, and nothing uses it after.
    unsafe fn terminate(state: NonNull<Self>);
}
