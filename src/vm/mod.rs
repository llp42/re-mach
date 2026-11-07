// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Virtual memory.

pub mod kernel;
pub mod map;
pub mod object;
pub mod page;
pub mod pageout;
pub mod pmap;
pub mod proxy;
pub mod user;

use pmap::MachinePmap as _;

use crate::arch;

bitflags::bitflags! {
    /// What memory allows, as Mach's interface numbers it.
    #[derive(PartialEq, Eq)]
    pub struct Protection: u32 {
        /// Reading it.
        const READ = 1;
        /// Writing it.
        const WRITE = 2;
        /// Running it.
        const EXECUTE = 4;
    }
}

/// What a child task gets of memory, as Mach's interface numbers it; only
/// the values used here.
#[derive(PartialEq, Eq)]
pub enum Inheritance {
    /// A copy of it.
    Copy = 1,
}

/// Sets up virtual memory before IPC, on the boot CPU.
pub fn bootstrap() {
    page::setup();
    object::bootstrap();
    map::init();
    kernel::init();
    arch::Pmap::init();
    crate::kalloc::init();
    page::init();
}

/// Finishes setting up virtual memory once IPC is up, on the boot CPU.
pub fn init() {
    object::init();
    proxy::init();
    page::print_pools();
}
