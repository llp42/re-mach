// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Copies between the kernel and user space.

use core::arch::asm;

use super::memory_layout::MAX_USER_ADDRESS;
use crate::error::Error;
use crate::smp;

/// Copies `source` to `destination` in the current task's space.
///
/// # Errors
///
/// `InvalidAddress` if `destination` is not below `MAX_USER_ADDRESS` or the
/// bytes would end past it, or if a page of them can't be faulted in.
///
/// # Panics
///
/// If no thread runs on the CPU.
pub fn copy_out(source: &[u8], destination: usize) -> Result<(), Error> {
    if destination >= MAX_USER_ADDRESS
        || destination
            .checked_add(source.len())
            .is_none_or(|end| end > MAX_USER_ADDRESS)
    {
        return Err(Error::InvalidAddress);
    }
    let thread = smp::current()
        .active_thread()
        .expect("copyout: no current thread");
    let faulted: u32;
    // SAFETY: the bytes go below kernel space; a fault there that can't be
    // resolved resumes at 2, set for this stack pointer and cleared after.
    unsafe {
        asm!(
            "movq %rsp, ({onfault_stack})",
            "leaq 2f(%rip), {recovery}",
            "movq {recovery}, ({onfault})",
            "rep movsb",
            "xorl {faulted:e}, {faulted:e}",
            "jmp 3f",
            "2:",
            "movl $1, {faulted:e}",
            "3:",
            "movq $0, ({onfault})",
            recovery = out(reg) _,
            faulted = out(reg) faulted,
            onfault = in(reg) thread.pcb().onfault.as_ptr(),
            onfault_stack = in(reg) thread.pcb().onfault_stack.as_ptr(),
            inout("rcx") source.len() => _,
            inout("rsi") source.as_ptr() => _,
            inout("rdi") destination => _,
            options(att_syntax, nostack),
        );
    }
    if faulted == 0 {
        Ok(())
    } else {
        Err(Error::InvalidAddress)
    }
}

/// Copies from `source` in the current task's space to `destination`.
///
/// # Errors
///
/// `InvalidAddress` if `source` is not below `MAX_USER_ADDRESS` or the
/// bytes would end past it, or if a page of them can't be faulted in.
///
/// # Panics
///
/// If no thread runs on the CPU.
pub fn copy_in(source: usize, destination: &mut [u8]) -> Result<(), Error> {
    if source >= MAX_USER_ADDRESS
        || source
            .checked_add(destination.len())
            .is_none_or(|end| end > MAX_USER_ADDRESS)
    {
        return Err(Error::InvalidAddress);
    }
    let thread = smp::current()
        .active_thread()
        .expect("copyin: no current thread");
    let faulted: u32;
    // SAFETY: the bytes come from below kernel space; a fault there that
    // can't be resolved resumes at 2, set for this stack pointer and
    // cleared after.
    unsafe {
        asm!(
            "movq %rsp, ({onfault_stack})",
            "leaq 2f(%rip), {recovery}",
            "movq {recovery}, ({onfault})",
            "rep movsb",
            "xorl {faulted:e}, {faulted:e}",
            "jmp 3f",
            "2:",
            "movl $1, {faulted:e}",
            "3:",
            "movq $0, ({onfault})",
            recovery = out(reg) _,
            faulted = out(reg) faulted,
            onfault = in(reg) thread.pcb().onfault.as_ptr(),
            onfault_stack = in(reg) thread.pcb().onfault_stack.as_ptr(),
            inout("rcx") destination.len() => _,
            inout("rsi") source => _,
            inout("rdi") destination.as_mut_ptr() => _,
            options(att_syntax, nostack),
        );
    }
    if faulted == 0 {
        Ok(())
    } else {
        Err(Error::InvalidAddress)
    }
}
