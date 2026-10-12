// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Firmware-independent output during single-CPU bootstrap.

use core::fmt::{Arguments, Write};

pub fn init() {
    crate::arch::serial::init();
}

/// Requires `init` and exclusive access to the early console.
pub fn print(args: Arguments<'_>) {
    let _ = Writer.write_fmt(args);
}

/// Emits an always-on failure diagnostic and stops the bootstrap CPU.
pub fn fatal(args: Arguments<'_>) -> ! {
    print(format_args!("re-mach: fatal: {args}\r\n"));
    crate::arch::halt()
}

struct Writer;

impl Write for Writer {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        crate::arch::serial::write(text.as_bytes());
        Ok(())
    }
}

/// Prints a diagnostic with its module, file and line after console init.
/// Arguments are not evaluated when debug assertions are disabled.
#[cfg(debug_assertions)]
macro_rules! dbg_println {
    () => { $crate::diagnostics::dbg_println!("") };
    ($($args:tt)*) => {
        $crate::diagnostics::print(core::format_args!(
            "[debug {} {}:{}] {}\r\n",
            module_path!(), file!(), line!(), core::format_args!($($args)*)
        ))
    };
}

#[cfg(not(debug_assertions))]
macro_rules! dbg_println {
    ($($args:tt)*) => {};
}

pub(crate) use dbg_println;
