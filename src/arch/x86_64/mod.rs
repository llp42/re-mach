// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! `x86_64` machine-dependent code.

mod acpi;
mod apic;
mod boot_memory;
mod boot_stack;
mod cpuid;
mod per_cpu;
mod pic;
mod serial;

use core::arch::asm;
use core::fmt::Write;

use x86_64::instructions::interrupts;
use x86_64::registers::rflags::{self, RFlags};

use crate::boot::{self, BootInfo};
use crate::smp::PerCpu as _;
use serial::Serial;

unsafe extern "C" {
    /// The kernel image's first byte, its DOS header.
    safe static __ImageBase: u8;
}

/// Machine-dependent initialization, once the boot source is done.
pub fn init<B: BootInfo>(boot_info: &B) -> ! {
    interrupts::disable();
    pic::disable();
    apic::enable();
    // SAFETY: own 16-byte-aligned stack; the callee never returns.
    unsafe {
        asm!(
            "movq {top}, %rsp",
            "callq {entry}",
            top = in(reg) boot_stack::top(),
            entry = sym init_on_boot_stack::<B>,
            in("rdi") core::ptr::from_ref(boot_info),
            options(noreturn, att_syntax),
        );
    }
}

/// Rest of `init`, on the boot stack.
extern "sysv64" fn init_on_boot_stack<B: BootInfo>(boot_info: *const B) -> ! {
    // SAFETY: the caller's frame on the abandoned entry stack is intact.
    let boot_info = unsafe { &*boot_info };
    // SAFETY: the boot CPU is CPU 0; nothing read `cpu_id` yet.
    unsafe { per_cpu::init(0) };
    // SAFETY: an empty RFLAGS keeps DF clear.
    unsafe { rflags::write(RFlags::empty()) };
    serial::init();
    boot::banner(&mut Serial);
    // SAFETY: the boot CPU, before any `features` call.
    unsafe { cpuid::discover() };
    let features = cpuid::features();
    let _ = write!(
        Serial,
        "cpu family {} model {} stepping {} pge {} pae {} sep {} fxsr {} xsave {}\r\n",
        features.family_id(),
        features.model_id(),
        features.stepping_id(),
        features.has_pge(),
        features.has_pae(),
        features.has_sysenter_sysexit(),
        features.has_fxsave_fxstor(),
        features.has_xsave()
    );
    let _ = write!(Serial, "cpu {}\r\n", per_cpu::PerCpu::cpu_id());
    // SAFETY: once, on the boot CPU; assumes the first 2 MiB and boot data are identity-mapped.
    unsafe { acpi::init(boot_info) };
    let image = (&raw const __ImageBase).addr();
    // SAFETY: the loader keeps the PE headers: `e_lfanew` at 0x3c, then
    // the PE32+ `SizeOfImage` 80 bytes past it.
    let size = unsafe {
        let e_lfanew = core::ptr::with_exposed_provenance::<u32>(image + 0x3c).read_unaligned();
        core::ptr::with_exposed_provenance::<u32>(image + usize::try_from(e_lfanew).unwrap() + 80)
            .read_unaligned()
    };
    let start = u64::try_from(image).unwrap();
    // SAFETY: the boot CPU; identity-mapped, so `start` is physical.
    unsafe { boot_memory::register_boot_data(start, start + u64::from(size), false) };
    for region in boot_info.memory_map() {
        let _ = write!(
            Serial,
            "memory area address {:#x} length {:#x} kind {:?}\r\n",
            region.address, region.length, region.kind
        );
    }
    halt();
}

pub fn halt() -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}
