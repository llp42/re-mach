// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Machine-dependent code, behind the trait machine-independent code uses.

use core::ffi::CStr;

use crate::boot::BootModule;
use crate::error::Error;

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "x86_64")]
pub use x86_64::{
    KERNEL_STACK_SIZE, MAX_KERNEL_ADDRESS, MAX_USER_ADDRESS, MIN_KERNEL_ADDRESS, MIN_USER_ADDRESS,
    PAGE_SIZE, Pcb, PerCpu, PhysAddr, Pmap, X86_64 as Current,
};

/// What machine-independent code asks of the machine.
pub trait Machine {
    /// Stops the current CPU for good.
    fn halt() -> !;

    /// Halts the current CPU, inside a critical section, until an interrupt
    /// arrives; at once if the section holds one back already.
    fn idle();

    /// Writes `text` to the console, where `\n` ends a line.
    fn write_console(text: &str);

    /// Sets up the machine once the VM and IPC are up, on the boot CPU:
    /// frees usable memory, sets up the FPU, finds the APICs and the CPUs,
    /// sets the APICs up, reads the time of day, and starts the time
    /// counter.
    fn init();
    /// Starts the current CPU's clock ticking.
    fn start_clock();
    /// The time counter: nanoseconds since it started.
    fn counter_nanoseconds() -> u64;
    /// Sets up tasks' machine part, on the boot CPU.
    fn task_module_init();
    /// The kernel's command line, as the loader gave it; empty if none.
    fn kernel_command_line() -> &'static CStr;

    /// The boot modules, in the loader's order.
    fn boot_modules() -> &'static [BootModule];

    /// Copies `source` to `destination` in the current task's space.
    ///
    /// # Errors
    ///
    /// `InvalidAddress` if `destination` is not below `MAX_USER_ADDRESS` or
    /// the bytes would end past it, or if a page of them can't be faulted in.
    ///
    /// # Panics
    ///
    /// If no thread runs on the CPU.
    fn copy_out(source: &[u8], destination: usize) -> Result<(), Error>;

    /// Copies from `source` in the current task's space to `destination`.
    ///
    /// # Errors
    ///
    /// `InvalidAddress` if `source` is not below `MAX_USER_ADDRESS` or the
    /// bytes would end past it, or if a page of them can't be faulted in.
    ///
    /// # Panics
    ///
    /// If no thread runs on the CPU.
    fn copy_in(source: usize, destination: &mut [u8]) -> Result<(), Error>;

    /// The EFI system table's physical address, when booted through UEFI.
    #[expect(dead_code, reason = "host_info reports it")]
    fn efi_system_table() -> Option<u64>;
}
