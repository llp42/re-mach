// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! x86_64 machine-dependent code.

mod gdt;
mod idt;
mod percpu;
mod serial;
mod trap;

use core::arch::asm;
use core::fmt::Write;

use x86_64::VirtAddr;
use x86_64::registers::model_specific::GsBase;

use crate::smp::PER_CPU_0;
use crate::uefi::MemoryMap;
pub use percpu::PerCpu;
use serial::Serial;

/// Machine-dependent initialization, after leaving boot services.
pub fn init(memory_map: MemoryMap) -> ! {
    serial::init();
    gdt::init(&PER_CPU_0);
    // SAFETY: nothing reads GS yet.
    unsafe { GsBase::write(VirtAddr::from_ptr(&PER_CPU_0)) };
    idt::init(&PER_CPU_0);
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
