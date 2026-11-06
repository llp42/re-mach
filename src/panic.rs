// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Kernel panics.

use core::panic::PanicInfo;

use crate::arch::{self, Machine as _};
use crate::sync::OnceLock;
use crate::{print, println, smp};

/// The first CPU to panic.
static PANICKING_CPU: OnceLock<usize> = OnceLock::new();

/// Reports the panic and stops this CPU; a CPU that panics after another
/// stops without a report.
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let cpu = smp::current().id();
    if PANICKING_CPU.set(cpu).is_err() && PANICKING_CPU.get().copied() != Some(cpu) {
        arch::Current::halt();
    }
    print!("panic {{cpu{cpu}}} ");
    if let Some(location) = info.location() {
        print!("{}:{}: ", location.file(), location.line());
    }
    println!("{}", info.message());
    arch::Current::halt()
}
