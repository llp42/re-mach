// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! `x86_64` machine-dependent code.

mod boot_memory;
mod cpu;
mod gdt;
mod icu;
mod idt;
mod instructions;
mod page_table;
mod per_cpu;
mod pmap;
mod port;
mod serial;
mod stack;
mod trap;
mod tss;

use core::arch::asm;
use core::fmt::Write;

use crate::smp::PER_CPU_0;
use crate::uefi::{self, MemoryMap};
pub use per_cpu::PerCpu;
use serial::Serial;
use stack::Stack;

/// Bytes per page.
pub const PAGE_SIZE: usize = 4096;

/// The boot CPU's own stack: four pages, as dfly's `bootstack`; the
/// firmware's stack is in boot-services memory.
static BOOT_STACK: Stack<{ PAGE_SIZE * 4 }> = Stack::new();

/// Machine-dependent initialization, after leaving boot services: moves to
/// `BOOT_STACK` and goes on in `init_on_boot_stack`.
pub fn init(mut memory_map: MemoryMap<'static>) -> ! {
    // SAFETY: nothing runs on `BOOT_STACK` yet; `init_on_boot_stack` takes
    // the map and never returns.
    unsafe {
        asm!(
            "mov {top}, %rsp",
            "xor %ebp, %ebp",
            "call {next}",
            top = in(reg) BOOT_STACK.top(),
            next = sym init_on_boot_stack,
            in("rdi") &raw mut memory_map,
            options(noreturn, att_syntax),
        );
    }
}

/// The rest of `init`, on `BOOT_STACK`.
extern "sysv64" fn init_on_boot_stack(memory_map: *mut MemoryMap<'static>) -> ! {
    // SAFETY: `init` handed its map over and never runs again.
    let memory_map = unsafe { memory_map.read() };
    serial::init();
    icu::init();
    for area in memory_map.clone() {
        let _ = write!(
            Serial,
            "memory area base {:#x} length {:#x} kind {}\r\n",
            area.physical_start,
            area.number_of_pages * uefi::PAGE_SIZE,
            area.ty
        );
    }
    boot_memory::init(memory_map);
    let md = &PER_CPU_0.md;
    let guards = [
        BOOT_STACK.guard(),
        md.double_fault_stack.guard(),
        md.debug_stack.guard(),
        md.machine_check_stack.guard(),
    ];
    // SAFETY: once, on the boot CPU, identity-mapped, with `boot_memory`
    // ready; `init_at_kernel_base` takes over.
    unsafe { pmap::bootstrap(init_at_kernel_base, &guards) }
}

/// The rest of the boot, at `KERNEL_BASE`.
extern "sysv64" fn init_at_kernel_base() -> ! {
    per_cpu::init(&PER_CPU_0);
    pmap::remove_identity();
    boot_memory::release_boot_services();
    halt();
}

pub fn halt() -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}
