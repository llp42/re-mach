// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! What the VM asks of the machine's page tables.

use core::ops::Range;

use super::Protection;
use crate::arch::PhysAddr;
use crate::error::Error;

/// Whether a mapping is wired: its page stays resident and mapped.
pub enum Wiring {
    /// Its page may be paged out.
    Pageable,
    /// It is wired.
    Wired,
}

/// The machine's physical maps.
pub trait MachinePmap {
    /// Takes `size` bytes of physical memory for good, before the page
    /// allocator runs; returns where the kernel reaches them.
    fn steal_memory(size: usize) -> *mut u8;

    /// The kernel's physical map.
    fn kernel() -> &'static Self;

    /// A new physical map for a user task, with one reference, sharing
    /// kernel space and mapping nothing else.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if it can't be allocated.
    fn create() -> Result<&'static Self, Error>;

    /// Takes a reference on it.
    fn reference(&self);

    /// Makes it the current CPU's, as the CPU switches to a thread of its
    /// task from one of another, inside a critical section.
    ///
    /// # Panics
    ///
    /// If it is the kernel's: not yet.
    fn activate(&self);
    /// Gives it up on the current CPU, as the CPU switches from a thread of
    /// its task to one of another, inside a critical section.
    ///
    /// # Panics
    ///
    /// If it is a user task's: not yet.
    fn deactivate(&self);

    /// Maps user `address` to the page at `physical`, allowing `protection`
    /// and wired as `wiring` says, adding the tables it lacks.
    ///
    /// # Panics
    ///
    /// If `address` is not a user address. If it is the kernel's, no page
    /// is free for a table, `address` maps a page already, `physical` is
    /// outside the page array, or for `Wiring::Wired`: not yet.
    fn enter(&self, address: usize, physical: PhysAddr, protection: Protection, wiring: Wiring);

    /// Cuts the mappings of user `range` down to `protection`, skipping the
    /// pages and tables not there.
    ///
    /// # Panics
    ///
    /// If it is the kernel's, `protection` lacks reading, or it is active on
    /// another CPU: not yet.
    fn protect(&self, range: Range<usize>, protection: Protection);

    /// Maps kernel `address` to the page at `physical`, read-write.
    fn enter_kernel(address: usize, physical: PhysAddr);
    /// The page `enter_kernel` mapped kernel `address` to; `None` if none.
    fn extract_kernel(address: usize) -> Option<PhysAddr>;
    /// Maps the kernel addresses of `address` to the device memory from
    /// `physical` on, uncached.
    fn enter_device(address: Range<usize>, physical: PhysAddr);
    /// The kernel addresses the boot mappings use.
    fn boot_space() -> Range<usize>;

    /// Sets up what physical maps need, once the kernel map is up.
    fn init();
}
