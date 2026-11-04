// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Machine-dependent code, behind the trait machine-independent code uses.

use core::ffi::CStr;

use crate::boot::BootModule;

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "x86_64")]
pub use x86_64::{
    MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE, PerCpu, PhysAddr, Pmap, X86_64 as Current,
};

/// What machine-independent code asks of the machine.
pub trait Machine {
    /// Stops the current CPU for good.
    fn halt() -> !;

    /// Writes `text` to the console, where `\n` ends a line.
    fn write_console(text: &str);

    /// Sets up the machine once the VM and IPC are up, on the boot CPU:
    /// frees usable memory.
    fn init();
    /// The kernel's command line, as the loader gave it; empty if none.
    #[expect(dead_code, reason = "the console and the bootstrap read it")]
    fn kernel_command_line() -> &'static CStr;

    /// The boot modules, in the loader's order.
    #[expect(dead_code, reason = "the bootstrap reads them")]
    fn boot_modules() -> &'static [BootModule];

    /// The EFI system table's physical address, when booted through UEFI.
    #[expect(dead_code, reason = "host_info reports it")]
    fn efi_system_table() -> Option<u64>;
}
