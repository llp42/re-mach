// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! x86_64 machine-dependent code.

mod serial;

use core::arch::asm;
use core::fmt::Write;

use x86_64::instructions::interrupts;

use crate::uefi::MemoryMap;
use serial::Serial;

/// Machine-dependent initialization, after leaving boot services.
pub fn init(memory_map: MemoryMap) -> ! {
    interrupts::disable();
    serial::init();
    for area in memory_map {
        let _ = write!(
            Serial,
            "memory area base {:#x} length {:#x} kind {}\r\n",
            area.physical_start,
            area.number_of_pages * 4096,
            area.ty
        );
    }
    halt();
}

pub fn halt() -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}
