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
pub trait MachinePhysMap {
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
    fn activate(&self);
    /// Gives it up on the current CPU, as the CPU switches from a thread of
    /// its task to one of another, inside a critical section, once the
    /// CPU's tables are another physical map's.
    fn deactivate(&self);

    /// Maps `address`, a kernel address in the kernel's physical map and a
    /// user one in another, to the page at `physical`, allowing `protection`
    /// and wired as `wiring` says, adding the tables a user one lacks.
    ///
    /// # Panics
    ///
    /// If `address` is not a kernel address in the kernel's, or not a user
    /// address in another. If `physical` is outside the page array: not
    /// yet. In the kernel's, if no table maps `address` or it maps a page
    /// already; for `protection` allowing running: not yet. In another, if
    /// no page is free for a table, `address` maps another page already, or
    /// if `address` maps `physical` already and it is active on another CPU:
    /// not yet.
    fn enter(&self, address: usize, physical: PhysAddr, protection: Protection, wiring: Wiring);

    /// Makes the mapping of `address` wired as `wiring` says.
    ///
    /// # Panics
    ///
    /// If no table maps `address`, or in the kernel's it is not a kernel
    /// address. For `Wiring::Wired`: not yet.
    fn change_wiring(&self, address: usize, wiring: Wiring);

    /// Removes the mappings of kernel `range` from the kernel's physical
    /// map, skipping the pages and tables not there.
    ///
    /// # Panics
    ///
    /// If `range` is not kernel addresses. In another physical map, or if
    /// another CPU runs: not yet.
    fn remove(&self, range: Range<usize>);

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
