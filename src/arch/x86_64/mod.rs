// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! x86_64 machine-dependent code.

mod apic;
mod boot_stack;
mod per_cpu;
mod pic;
mod serial;

use core::arch::asm;
use core::fmt::Write;

use x86_64::instructions::interrupts;

use crate::smp::PerCpu as _;
use crate::uefi::MemoryMap;
use serial::Serial;

/// Machine-dependent initialization, after leaving boot services.
#[expect(
    clippy::needless_pass_by_value,
    reason = "passed to the boot stack by address"
)]
pub fn init(memory_map: MemoryMap) -> ! {
    interrupts::disable();
    pic::disable();
    apic::enable();
    // SAFETY: own 16-byte-aligned stack; the callee never returns.
    unsafe {
        asm!(
            "movq {top}, %rsp",
            "callq {entry}",
            top = in(reg) boot_stack::top(),
            entry = sym init_on_boot_stack,
            in("rdi") &raw const memory_map,
            options(noreturn, att_syntax),
        );
    }
}

/// Rest of `init`, on the boot stack.
extern "sysv64" fn init_on_boot_stack(memory_map: *const MemoryMap) -> ! {
    // SAFETY: `init`'s frame on the abandoned firmware stack is intact.
    let memory_map = unsafe { memory_map.read() };
    // SAFETY: the boot CPU is CPU 0; nothing read `cpu_id` yet.
    unsafe { per_cpu::init(0) };
    serial::init();
    let _ = write!(Serial, "cpu {}\r\n", per_cpu::PerCpu::cpu_id());
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
