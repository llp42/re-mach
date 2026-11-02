// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! System calls: their entry point.

use core::arch::global_asm;

use x86_64::VirtAddr;
use x86_64::registers::model_specific::{Efer, EferFlags, LStar, SFMask, Star};
use x86_64::registers::rflags::RFlags;

use super::gdt;

// TODO: the Mach trap path, at the first user-mode step.
global_asm!(
    ".pushsection .text, \"ax\"",
    ".globl syscall_entry",
    "syscall_entry:",
    "ud2",
    ".popsection",
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
