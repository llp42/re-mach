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
use core::cell::UnsafeCell;
use core::ffi::CStr;
use core::fmt::Write;
use core::{ptr, slice};

use x86_64::registers::rflags::{self, RFlags};

use crate::boot;
use crate::boot::multiboot2::{self, ElfSectionType, Tag};
use crate::smp::PerCpu as _;
use address::PhysAddr;
use boot_memory::Retention;
use memory_layout::{MIN_KERNEL_ADDRESS, PAGE_SIZE, phys_to_virt};
use serial::Serial;

struct KernelCommandLine(UnsafeCell<&'static CStr>);

// SAFETY: written once by `init`, before any reader.
unsafe impl Sync for KernelCommandLine {}

/// The kernel's command line; empty until `init` copies the loader's.
static KERNEL_COMMAND_LINE: KernelCommandLine = KernelCommandLine(UnsafeCell::new(c""));

/// A boot module, copied into boot memory.
#[expect(dead_code, reason = "the bootstrap reads it")]
pub struct BootModule {
    /// Its physical range, `end` past its last byte.
    pub start: PhysAddr,
    pub end: PhysAddr,
    pub command_line: &'static CStr,
}

struct BootModules(UnsafeCell<&'static [BootModule]>);

// SAFETY: written once by `init`, before any reader.
unsafe impl Sync for BootModules {}

/// The boot modules; empty until `init` copies the loader's.
static BOOT_MODULES: BootModules = BootModules(UnsafeCell::new(&[]));

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
            Tag::Module {
                mod_start, mod_end, ..
            } => {
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
    // SAFETY: the boot CPU, once, after boot memory's bootstrap.
    unsafe { copy_boot_info(boot_info) };
    halt();
}

/// Copies into boot memory what outlives the boot information: the
/// kernel's command line, and the modules with theirs.
///
/// # Safety
///
/// Runs once, on the boot CPU, after boot memory's bootstrap.
unsafe fn copy_boot_info(boot_info: multiboot2::Info) {
    let count = boot_info
        .tags()
        .filter(|tag| matches!(tag, Tag::Module { .. }))
        .count();
    let modules = if count == 0 {
        ptr::NonNull::dangling().as_ptr()
    } else {
        // SAFETY: guaranteed by the caller.
        phys_to_virt::<BootModule>(unsafe { alloc_bytes(count * size_of::<BootModule>()) })
    };
    let mut len = 0;
    for tag in boot_info.tags() {
        match tag {
            Tag::Cmdline(cmdline) => {
                let bytes = cmdline.to_bytes_with_nul();
                // SAFETY: guaranteed by the caller.
                let copy = phys_to_virt::<u8>(unsafe { alloc_bytes(bytes.len()) });
                // SAFETY: fresh boot memory, mapped at the kernel map base;
                // then NUL-terminated and never written again.
                unsafe {
                    copy.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
                    *KERNEL_COMMAND_LINE.0.get() = CStr::from_ptr(copy.cast());
                }
            }
            Tag::Module {
                mod_start,
                mod_end,
                cmdline,
            } => {
                let size = usize::try_from(mod_end - mod_start).unwrap();
                // SAFETY: guaranteed by the caller.
                let start = unsafe { alloc_bytes(size) };
                let copy = phys_to_virt::<u8>(start);
                let module =
                    ptr::with_exposed_provenance::<u8>(usize::try_from(mod_start).unwrap());
                let bytes = cmdline.to_bytes_with_nul();
                // SAFETY: guaranteed by the caller.
                let string = phys_to_virt::<u8>(unsafe { alloc_bytes(bytes.len()) });
                // SAFETY: the module is below 4 GiB, identity-mapped, and
                // temporary boot data, so apart from fresh boot memory; the
                // copies are then never written again.
                unsafe {
                    copy.copy_from_nonoverlapping(module, size);
                    string.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
                    modules.add(len).write(BootModule {
                        start,
                        end: start + u64::try_from(size).unwrap(),
                        command_line: CStr::from_ptr(string.cast()),
                    });
                }
                len += 1;
            }
            _ => {}
        }
    }
    // SAFETY: `len` modules written, in boot memory never written again.
    unsafe { *BOOT_MODULES.0.get() = slice::from_raw_parts(modules, len) };
}

/// The kernel's command line, as the loader gave it; empty if none.
#[expect(dead_code, reason = "the console and the bootstrap read it")]
pub fn kernel_command_line() -> &'static CStr {
    // SAFETY: written only by `init`, before any reader.
    unsafe { *KERNEL_COMMAND_LINE.0.get() }
}

/// The boot modules, in the loader's order.
#[expect(dead_code, reason = "the bootstrap reads them")]
pub fn boot_modules() -> &'static [BootModule] {
    // SAFETY: written only by `init`, before any reader.
    unsafe { *BOOT_MODULES.0.get() }
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
unsafe fn alloc_bytes(size: usize) -> PhysAddr {
    let page_size = usize::try_from(PAGE_SIZE).unwrap();
    // SAFETY: guaranteed by the caller.
    unsafe { boot_memory::alloc_pages(size.div_ceil(page_size)) }
}
