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

use crate::boot;
use crate::boot::multiboot2::{self, ElfSectionType, Tag};
use crate::smp::PerCpu as _;
use address::PhysAddr;
use boot_memory::Retention;
use memory_layout::{MIN_KERNEL_ADDRESS, PAGE_SIZE};
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
    let info = boot_info.range();
    let start = u64::try_from(info.start).unwrap();
    let end = u64::try_from(info.end).unwrap();
    // SAFETY: the boot CPU; the boot information, until no longer needed.
    unsafe {
        boot_memory::register_boot_data(PhysAddr(start)..PhysAddr(end), Retention::Temporary);
    }
    for tag in boot_info.tags() {
        match tag {
            Tag::ElfSections(sections) => {
                for section in sections.filter(|section| {
                    matches!(section.ty, ElfSectionType::SYMTAB | ElfSectionType::STRTAB)
                        && section.size != 0
                }) {
                    let (start, end) = (section.addr, section.addr + section.size);
                    // SAFETY: the boot CPU; a symbol or string table, kept.
                    unsafe {
                        boot_memory::register_boot_data(
                            PhysAddr(start)..PhysAddr(end),
                            Retention::Permanent,
                        );
                    }
                }
            }
            Tag::Module { mod_start, mod_end } => {
                let (start, end) = (u64::from(mod_start), u64::from(mod_end));
                // SAFETY: the boot CPU; a module, until no longer needed.
                unsafe {
                    boot_memory::register_boot_data(
                        PhysAddr(start)..PhysAddr(end),
                        Retention::Temporary,
                    );
                }
            }
            _ => {}
        }
    }
    // SAFETY: the boot CPU, once.
    unsafe { boot_memory::bootstrap(boot_info) };
    halt();
}

pub fn halt() -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}

/// Boot memory for `size` bytes, rounded up to whole pages.
///
/// # Panics
///
/// If `size` is 0 or more than the boot heap has left.
///
/// # Safety
///
/// Runs on the boot CPU only, after boot memory's bootstrap.
#[expect(dead_code, reason = "the command line copy calls it")]
unsafe fn alloc_bytes(size: usize) -> PhysAddr {
    let page_size = usize::try_from(PAGE_SIZE).unwrap();
    // SAFETY: guaranteed by the caller.
    unsafe { boot_memory::alloc_pages(size.div_ceil(page_size)) }
}
