// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Firmware entry and fatal exception reporting.

#![no_std]
#![no_main]

mod arch;
mod boot;
mod diagnostics;
mod kernel;

#[cfg(target_os = "uefi")]
#[uefi::entry]
fn main() -> uefi::Status {
    diagnostics::init();
    boot::uefi::enter()
}

#[cfg(not(target_os = "uefi"))]
compile_error!("no boot adapter implemented for this target");

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    diagnostics::fatal(format_args!("panic: {info}"))
}
