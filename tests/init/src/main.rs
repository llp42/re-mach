// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! The first user-space program: writes a greeting to the console and exits.
//!
//! It runs as the init process, with no arguments, environment or open
//! files, and makes the kernel's native system calls directly: `syscall`
//! with the call number in `%rax` and arguments in `%rdi`, `%rsi` and
//! `%rdx`. A failed call sets the carry flag and returns the error number in
//! `%rax`. The exit status is 0 on success and the error number otherwise.

#![no_std]
#![no_main]

use core::arch::asm;
use core::ffi::CStr;

const SYS_EXIT: u64 = 1;
const SYS_WRITE: u64 = 4;
const SYS_OPEN: u64 = 5;
const WRITE_ONLY: u64 = 0x0001;
const CONSOLE: &CStr = c"/console";
const GREETING: &[u8] = b"re-mach: hello, world\n";

#[unsafe(no_mangle)]
extern "C" fn _start() -> ! {
    match greet() {
        Ok(()) => exit(0),
        Err(error) => exit(error),
    }
}

fn greet() -> Result<(), u64> {
    let console = syscall3(SYS_OPEN, CONSOLE.as_ptr() as u64, WRITE_ONLY, 0)?;
    syscall3(
        SYS_WRITE,
        console,
        GREETING.as_ptr() as u64,
        GREETING.len() as u64,
    )?;
    Ok(())
}

fn exit(status: u64) -> ! {
    // SAFETY: exit reads only its status argument and does not return.
    unsafe {
        asm!(
            "syscall",
            in("rax") SYS_EXIT,
            in("rdi") status,
            options(noreturn, nostack, att_syntax),
        )
    }
}

/// Makes a system call with up to three arguments and returns its result or
/// its error number.
fn syscall3(number: u64, first: u64, second: u64, third: u64) -> Result<u64, u64> {
    let result: u64;
    let failed: u8;
    // SAFETY: the kernel only reads the argument registers and the user
    // memory they describe, which it validates itself. `syscall` clobbers
    // %rcx and %r11, and the kernel's return path also clobbers %rdx and
    // %r10.
    unsafe {
        asm!(
            "syscall",
            "setc {failed}",
            inlateout("rax") number => result,
            in("rdi") first,
            in("rsi") second,
            inlateout("rdx") third => _,
            lateout("rcx") _,
            lateout("r10") _,
            lateout("r11") _,
            failed = lateout(reg_byte) failed,
            options(nostack, att_syntax),
        )
    }
    if failed != 0 { Err(result) } else { Ok(result) }
}

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo) -> ! {
    exit(255)
}
