// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Virtual memory.

pub mod fault;
pub mod kernel;
pub mod map;
pub mod object;
pub mod page;
pub mod pageout;
pub mod phys_map;
pub mod proxy;
pub mod user;

use core::ops::{Add, Sub};
use core::sync::atomic::AtomicU32;

use phys_map::MachinePhysMap as _;

use crate::arch;

bitflags::bitflags! {
    /// What memory allows, as Mach's interface numbers it.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct Protection: u32 {
        /// Reading it.
        const READ = 1;
        /// Writing it.
        const WRITE = 2;
        /// Running it.
        const EXECUTE = 4;
    }
}

bitflags::bitflags! {
    /// What wiring a whole map covers, as Mach's interface numbers it; none
    /// unwires it.
    #[derive(Clone, Copy)]
    pub struct Wire: u32 {
        /// What it maps now.
        const CURRENT = 1;
        /// What it maps later.
        const FUTURE = 2;
    }
}

/// A virtual address, as maps hold them.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtualAddress(usize);

impl VirtualAddress {
    /// Rounds down to a multiple of `align`, a power of two.
    pub const fn align_down(self, align: usize) -> Self {
        Self(self.0 & !(align - 1))
    }

    /// Rounds up to a multiple of `align`.
    pub const fn next_multiple_of(self, align: usize) -> Self {
        Self(self.0.next_multiple_of(align))
    }

    /// `self` plus `size`; `None` on overflow.
    pub fn checked_add(self, size: usize) -> Option<Self> {
        self.0.checked_add(size).map(Self)
    }
}

impl From<usize> for VirtualAddress {
    fn from(address: usize) -> Self {
        Self(address)
    }
}

impl From<VirtualAddress> for usize {
    fn from(address: VirtualAddress) -> Self {
        address.0
    }
}

impl Add<usize> for VirtualAddress {
    type Output = Self;

    fn add(self, size: usize) -> Self {
        Self(self.0 + size)
    }
}

impl Sub for VirtualAddress {
    type Output = usize;

    /// The bytes from `other` up to `self`.
    fn sub(self, other: Self) -> usize {
        self.0 - other.0
    }
}

/// What a child task gets of memory; only the values used here.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Inheritance {
    /// A copy of it.
    Copy,
    /// Nothing of it.
    None,
}

/// The VM's event counts, as Mach's `vm_statistics` reports them; only
/// those kept so far.
struct Statistics {
    /// Faults: each search for a faulting page counts one.
    faults: AtomicU32,
    /// Pages filled with zeros on a fault.
    zero_fill_count: AtomicU32,
    /// Pages asked of a pager on a fault.
    pageins: AtomicU32,
    /// Pages copied up from a shadow object on a write fault.
    cow_faults: AtomicU32,
    /// Lookups of a memory object's object.
    lookups: AtomicU32,
    /// Lookups that found an object.
    #[expect(dead_code, reason = "vm_object_enter's hit counts it")]
    hits: AtomicU32,
}

/// The VM's event counts.
static STATISTICS: Statistics = Statistics {
    faults: AtomicU32::new(0),
    zero_fill_count: AtomicU32::new(0),
    pageins: AtomicU32::new(0),
    cow_faults: AtomicU32::new(0),
    lookups: AtomicU32::new(0),
    hits: AtomicU32::new(0),
};

/// Sets up virtual memory before IPC, on the boot CPU.
pub fn bootstrap() {
    page::setup();
    object::bootstrap();
    map::init();
    kernel::init();
    arch::PhysMap::init();
    crate::kalloc::init();
    page::init();
}

/// Finishes setting up virtual memory once IPC is up, on the boot CPU.
pub fn init() {
    object::init();
    proxy::init();
    page::print_pools();
}
