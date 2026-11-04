// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads' machine state: their process control blocks.

use super::fpu;
use super::trap::TrapFrame;
use crate::kernel::Error;
use crate::kernel::slab::{Cache, CacheBox};
use crate::kernel::thread::MachineThread;

/// A thread's machine state; so far only its user registers.
#[repr(C, align(16))]
pub struct Pcb {
    /// Its user registers, laid out as a trap from user mode saves them.
    user: TrapFrame,
}

/// The cache process control blocks come from.
static CACHE: Cache<Pcb> = Cache::new("pcb");

impl MachineThread for Pcb {
    fn module_init() {
        CACHE.register();
        fpu::module_init();
    }

    fn new() -> Result<CacheBox<Self>, Error> {
        CACHE.allocate(Self {
            user: TrapFrame::user(),
        })
    }
}
