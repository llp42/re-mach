// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Console output.

use core::fmt::{self, Write};

use crate::arch::{self, Machine as _};

/// The machine's console, as a `fmt::Write`.
struct Console;

impl Write for Console {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        arch::Current::write_console(text);
        Ok(())
    }
}

/// Writes `args` to the console; `print!` and `println!` call it.
pub fn print(args: fmt::Arguments) {
    let _ = Console.write_fmt(args);
}

/// Prints to the console.
#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => {
        $crate::kernel::console::print(format_args!($($arg)*))
    };
}

/// Prints to the console, then ends the line.
#[macro_export]
macro_rules! println {
    ($($arg:tt)*) => {
        $crate::print!("{}\n", format_args!($($arg)*))
    };
}
