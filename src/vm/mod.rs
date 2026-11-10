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

/// What a child task gets of memory; only the values used here.
#[derive(PartialEq, Eq)]
pub enum Inheritance {
    /// A copy of it.
    Copy,
}

/// The VM's event counts, as Mach's `vm_statistics` reports them; only
/// those kept so far.
struct Statistics {
    /// Faults: each search for a faulting page counts one.
    faults: AtomicU32,
    /// Pages filled with zeros on a fault.
    zero_fill_count: AtomicU32,
}

/// The VM's event counts.
static STATISTICS: Statistics = Statistics {
    faults: AtomicU32::new(0),
    zero_fill_count: AtomicU32::new(0),
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
