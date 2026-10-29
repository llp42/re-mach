// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Boot information, read in place from the firmware or boot loader.

pub mod uefi;

use core::fmt::Write;

/// One memory-map entry.
#[derive(Clone, Copy)]
pub struct MemoryRegion {
    pub address: u64,
    pub length: u64,
    pub kind: MemoryKind,
}

/// What a memory region holds; Multiboot2's memory types.
#[derive(Clone, Copy, Debug)]
pub enum MemoryKind {
    Available,
    Reserved,
    AcpiReclaimable,
    Nvs,
    BadRam,
}

/// Boot data; each source implements it over its original data
/// (UEFI, Multiboot2).
pub trait BootInfo {
    /// Physical address of the ACPI RSDP, newest revision first.
    fn acpi_rsdp(&self) -> Option<u64>;
    /// Physical address of the EFI system table.
    fn efi_system_table(&self) -> Option<u64>;
    fn memory_map(&self) -> impl Iterator<Item = MemoryRegion>;
}

/// Prints the kernel's name and version.
pub fn banner(out: &mut impl Write) {
    let _ = write!(
        out,
        "{} {}\r\n",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );
}
