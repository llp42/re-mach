// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! The re-mach kernel.
//!
//! The kernel is the imported C code, which build.rs compiles; rustc links
//! it together with this crate. A Multiboot loader enters it at the
//! architecture's entry code, which translates the loader's information
//! into the C kernel's handoff and jumps to the C `btext`, so there is no
//! Rust `main`. C calls into Rust through `extern "C"` functions exported
//! with `#[unsafe(no_mangle)]`; Rust calls into C through `ffi`.
//!
//! The code is built for `x86_64-unknown-none` with the kernel code model
//! and no FPU/SIMD, like the C code (see .cargo/config.toml). Only `core`
//! is available: there is no allocator yet.

#![no_std]
#![no_main]

mod arch;
mod boot;
mod console;
mod diagnostics;
mod ffi;
mod init;

use core::ffi::c_void;
use core::fmt::Write;

/// Runs once during boot, after the console is up, by the SYSINIT in
/// glue.c.
#[unsafe(no_mangle)]
pub extern "C" fn rust_init(_arg: *mut c_void) {
    // SAFETY: a NUL-terminated format string without conversions.
    unsafe { ffi::kprintf(c"re-mach: kernel Rust initialized\n".as_ptr()) };
}

/// Rust panics end in the kernel's panic, with the message and location.
/// Before the kernel's console exists nothing would print it, so the boot
/// entry code reports failures with `diagnostics::fatal` instead.
#[panic_handler]
fn on_panic(info: &core::panic::PanicInfo) -> ! {
    let mut msg = ffi::CBuf::<256>::new();
    let _ = write!(msg, "{info}");
    // SAFETY: "%s" with a NUL-terminated string.
    unsafe { ffi::panic(c"rust: %s".as_ptr(), msg.as_ptr()) }
}
