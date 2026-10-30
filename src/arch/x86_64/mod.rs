// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! `x86_64` machine-dependent code.

mod acpi;
mod address;
mod ap_boot;
mod apic;
mod boot_header;
mod boot_memory;
mod boot_stack;
mod cpuid;
mod memory_layout;
mod per_cpu;
mod pic;
mod serial;

use core::arch::asm;
use core::fmt::Write;

use x86_64::registers::rflags::{self, RFlags};

use crate::boot::{self, multiboot2};
use crate::smp::PerCpu as _;
use address::PhysAddr;
use boot_memory::Retention;
use memory_layout::MIN_KERNEL_ADDRESS;
use serial::Serial;

unsafe extern "C" {
    /// The image's first byte, in `.boot`, which runs where it is loaded.
    safe static boot_start: u8;
    /// Past the image's last byte, at the kernel map base.
    safe static _end: u8;
}

/// Machine-dependent initialization, from `boot_header` on the boot stack;
/// `info_address` is the boot information's physical address.
extern "sysv64" fn init(info_address: usize, magic: u32) -> ! {
    assert_eq!(
        magic,
        multiboot2::BOOTLOADER_MAGIC,
        "Invalid multiboot magic"
    );
    // SAFETY: the loader's structure, below 4 GiB and identity-mapped;
    // nothing writes it before boot memory releases it.
    let boot_info = unsafe { multiboot2::Info::from_address(info_address) };
    pic::disable();
    apic::enable();
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
    // SAFETY: once, on the boot CPU; the first 4 GiB are identity-mapped.
    unsafe { acpi::init(boot_info) };
    let start = u64::try_from((&raw const boot_start).addr()).unwrap();
    let end = u64::try_from((&raw const _end).addr()).unwrap() - MIN_KERNEL_ADDRESS;
    // SAFETY: the boot CPU; from `.boot`, which holds the live page tables.
    unsafe {
        boot_memory::register_boot_data(PhysAddr(start)..PhysAddr(end), Retention::Permanent);
    }
    // SAFETY: the boot CPU, once.
    unsafe { boot_memory::bootstrap(boot_info) };
    for entry in boot_info.mmap().into_iter().flatten() {
        let _ = write!(
            Serial,
            "memory area address {:#x} length {:#x} kind {:?}\r\n",
            entry.base_addr, entry.length, entry.ty
        );
    }
    halt();
}

pub fn halt() -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}
