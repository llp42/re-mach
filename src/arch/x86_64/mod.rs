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
mod counter;
mod cpuid;
mod fpu;
mod gdt;
mod hpet;
mod idt;
mod interrupt;
mod memory_layout;
mod mmio;
mod per_cpu;
mod pic;
mod pmap;
mod rtc;
mod serial;
mod stack;
mod syscall;
mod trap;
mod tss;

use core::arch::asm;
use core::ffi::CStr;
use core::fmt::Write;
use core::mem::MaybeUninit;
use core::{ptr, slice};

use x86_64::registers::control::{Cr0, Cr0Flags};
use x86_64::registers::rflags::{self, RFlags};

use crate::arch::Machine;
use crate::boot::multiboot2::{self, ElfSectionType, Tag};
use crate::boot::{self, BootModule};
use crate::kernel::clock::{self, TimeValue};
use crate::kernel::machine::{self, CpuSubtype, CpuType, Slot};
use crate::kernel::startup;
use crate::println;
use crate::smp::{self, MachinePerCpu as _};
use crate::sync::OnceLock;
pub use address::PhysAddr;
use boot_memory::Retention;
use memory_layout::KERNEL_BASE;
pub use memory_layout::{MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE};
pub use per_cpu::PerCpu;
pub use pmap::Pmap;
use serial::Serial;

/// The kernel's command line, once `init` copies the loader's.
static KERNEL_COMMAND_LINE: OnceLock<&'static CStr> = OnceLock::new();

/// The boot modules, once `init` copies the loader's.
static BOOT_MODULES: OnceLock<&'static [BootModule]> = OnceLock::new();

unsafe extern "C" {
    /// The image's first byte, in `.boot`, which runs where it is loaded.
    safe static boot_start: u8;
    /// Past the image's last byte, at the kernel map base.
    safe static _end: u8;
}

/// Machine-dependent initialization, from `boot_header` on the boot stack;
/// `info_address` is the boot information's physical address.
extern "sysv64" fn init(info_address: usize, magic: u32) -> ! {
    // SAFETY: the boot CPU is CPU 0; nothing read `cpu_id` yet.
    unsafe { per_cpu::init(0) };
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
    // SAFETY: an empty RFLAGS keeps DF clear.
    unsafe { rflags::write(RFlags::empty()) };
    Serial::init();
    boot::banner();
    cpuid::discover();
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
    let _ = write!(Serial, "cpu {}\r\n", smp::current().id());
    // SAFETY: once, on the boot CPU; the first 4 GiB are identity-mapped.
    unsafe { acpi::init(boot_info) };
    let start = (&raw const boot_start).addr();
    let end = (&raw const _end).addr() - KERNEL_BASE;
    // From `.boot`, which holds the live page tables.
    boot_memory::register_boot_data(PhysAddr(start)..PhysAddr(end), Retention::Permanent);
    let info = boot_info.range();
    // The boot information, until no longer needed.
    boot_memory::register_boot_data(
        PhysAddr(info.start)..PhysAddr(info.end),
        Retention::Temporary,
    );
    for tag in boot_info.tags() {
        match tag {
            Tag::ElfSections(sections) => {
                for section in sections.filter(|section| {
                    matches!(section.ty, ElfSectionType::SYMTAB | ElfSectionType::STRTAB)
                        && section.size != 0
                }) {
                    let (start, end) = (section.addr, section.addr + section.size);
                    // A symbol or string table, kept.
                    boot_memory::register_boot_data(
                        PhysAddr(start)..PhysAddr(end),
                        Retention::Permanent,
                    );
                }
            }
            Tag::Module {
                mod_start, mod_end, ..
            } => {
                // A module, until no longer needed.
                boot_memory::register_boot_data(
                    PhysAddr(mod_start)..PhysAddr(mod_end),
                    Retention::Temporary,
                );
            }
            _ => {}
        }
    }
    boot_memory::bootstrap(boot_info);
    // SAFETY: the modules lie below 4 GiB, which is identity-mapped.
    unsafe { copy_boot_info(boot_info) };
    // SAFETY: the boot CPU, once, after boot memory's bootstrap.
    unsafe { pmap::bootstrap() };
    boot_memory::setup();
    // SAFETY: turns caching on; paging and protection stay as they are.
    unsafe {
        Cr0::update(|flags| {
            flags.remove(Cr0Flags::CACHE_DISABLE | Cr0Flags::NOT_WRITE_THROUGH);
        });
    }
    // SAFETY: the boot CPU, once.
    unsafe { gdt::init() };
    // SAFETY: the boot CPU, once, after the GDT.
    unsafe { idt::init() };
    // SAFETY: the boot CPU, once, after the IDT.
    unsafe { interrupt::init() };
    // SAFETY: the boot CPU, once, after the GDT.
    unsafe { gdt::store_user_segments() };
    // SAFETY: the boot CPU, once, after the user segments.
    unsafe { syscall::init() };
    // SAFETY: the boot CPU, once, after the GDT and its per-CPU data.
    unsafe { tss::init() };
    PerCpu::store_apic_id();
    PerCpu::current().store_gdt(&gdt::GDT);
    machine::store_slot(
        0,
        Slot {
            ty: CpuType::X86_64,
            subtype: CpuSubtype::AT386,
        },
    );
    startup::start()
}

/// Copies into boot memory what outlives the boot information: the
/// kernel's command line, and the modules with theirs.
///
/// # Safety
///
/// The modules are identity-mapped and readable.
unsafe fn copy_boot_info(boot_info: multiboot2::Info) {
    let count = boot_info
        .tags()
        .filter(|tag| matches!(tag, Tag::Module { .. }))
        .count();
    let modules = boot_slice::<BootModule>(count);
    let (mut command_line, mut slots) = (None, modules.iter_mut());
    for tag in boot_info.tags() {
        match tag {
            Tag::Cmdline(cmdline) => command_line = Some(copy_c_string(cmdline)),
            Tag::Module {
                mod_start,
                mod_end,
                cmdline,
            } => {
                let size = mod_end - mod_start;
                let start = alloc_bytes(size);
                // SAFETY: the module is identity-mapped, and temporary boot
                // data, so apart from fresh boot memory.
                unsafe {
                    start
                        .kernel_window_ptr::<u8>()
                        .copy_from_nonoverlapping(ptr::with_exposed_provenance(mod_start), size);
                }
                let module = BootModule {
                    range: start..start + size,
                    command_line: copy_c_string(cmdline),
                };
                slots.next().expect("a module not counted").write(module);
            }
            _ => {}
        }
    }
    if let Some(command_line) = command_line {
        assert!(
            KERNEL_COMMAND_LINE.set(command_line).is_ok(),
            "kernel command line copied twice"
        );
    }
    // SAFETY: one slot was written for each module counted.
    let modules = unsafe { slice::from_raw_parts(modules.as_ptr().cast(), modules.len()) };
    assert!(
        BOOT_MODULES.set(modules).is_ok(),
        "boot modules copied twice"
    );
}

/// A copy of `string` in boot memory.
fn copy_c_string(string: &CStr) -> &'static CStr {
    let bytes = string.to_bytes_with_nul();
    let copy = alloc_bytes(bytes.len()).kernel_window_ptr::<u8>();
    // SAFETY: fresh boot memory, mapped at the kernel window; then
    // NUL-terminated and never written again.
    unsafe {
        copy.copy_from_nonoverlapping(bytes.as_ptr(), bytes.len());
        CStr::from_ptr(copy.cast())
    }
}

/// Room for `len` items in boot memory.
fn boot_slice<T>(len: usize) -> &'static mut [MaybeUninit<T>] {
    if len == 0 {
        return &mut [];
    }
    let start = alloc_bytes(len * size_of::<T>()).kernel_window_ptr();
    // SAFETY: fresh boot memory, page-aligned and mapped at the kernel
    // window; nothing else refers to it.
    unsafe { slice::from_raw_parts_mut(start, len) }
}

/// Sets the time of day from the real-time clock, on the boot CPU.
fn init_time_of_day() {
    if let Some(date) = rtc::read() {
        println!(
            "RTC time is {:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            date.year, date.month, date.day, date.hour, date.minute, date.second
        );
        clock::set_time(TimeValue {
            seconds: date.seconds(),
            nanoseconds: 0,
        });
    }
}

/// Gives each CPU the ACPI tables list but the boot CPU its slot, the
/// boot CPU's kind, on the boot CPU.
fn init_slots() {
    let count = apic::APIC_IDS.get().map_or(1, |ids| ids.len());
    for cpu in 1..count {
        machine::store_slot(
            cpu,
            Slot {
                ty: CpuType::X86_64,
                subtype: CpuSubtype::AT386,
            },
        );
    }
}

/// The `x86_64` machine.
pub struct X86_64;

impl Machine for X86_64 {
    fn halt() -> ! {
        halt()
    }

    fn write_console(text: &str) {
        // A serial terminal needs a carriage return before each line feed.
        for (index, line) in text.split('\n').enumerate() {
            if index > 0 {
                let _ = Serial.write_str("\r\n");
            }
            let _ = Serial.write_str(line);
        }
    }

    fn init() {
        boot_memory::free_usable();
        fpu::init();
        if let Err(error) = acpi::init_apics() {
            panic!("acpi_apic_init failed with {error:?}");
        }
        init_slots();
        apic::configure_io_apics();
        init_time_of_day();
        hpet::init();
        counter::init();
    }

    fn kernel_command_line() -> &'static CStr {
        KERNEL_COMMAND_LINE.get().copied().unwrap_or_default()
    }

    fn boot_modules() -> &'static [BootModule] {
        BOOT_MODULES.get().copied().unwrap_or_default()
    }

    fn efi_system_table() -> Option<u64> {
        acpi::EFI_SYSTEM_TABLE.get().copied()
    }
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
fn alloc_bytes(size: usize) -> PhysAddr {
    boot_memory::alloc_pages(size.div_ceil(PAGE_SIZE))
}
