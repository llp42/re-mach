// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! `x86_64` machine-dependent code.

mod gdt;
mod idt;
mod instructions;
mod per_cpu;
mod port;
mod serial;
mod trap;

use core::arch::asm;
use core::fmt::Write;

use crate::smp::PER_CPU_0;
use crate::uefi::MemoryMap;
pub use per_cpu::PerCpu;
use serial::Serial;

/// Machine-dependent initialization, after leaving boot services.
pub fn init(memory_map: MemoryMap) -> ! {
    serial::init();
    gdt::init(&PER_CPU_0);
    // SAFETY: nothing reads GS yet.
    unsafe {
        instructions::write_msr(
            instructions::IA32_GS_BASE,
            u64::try_from((&raw const PER_CPU_0).expose_provenance()).unwrap(),
        );
    }
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
