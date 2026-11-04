// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Machine-independent startup.

use super::{clock, machine, processor, radix_tree, sched, task, thread};
use crate::arch::{self, Machine as _};
use crate::{ipc, vm};

/// Starts the machine-independent kernel, on the boot CPU, once the
/// machine is set up.
pub fn start() -> ! {
    sched::init();
    vm::bootstrap();
    radix_tree::init();
    ipc::bootstrap();
    vm::init();
    ipc::init();
    arch::Current::init();
    clock::map_time();
    machine::init_info();
    task::init();
    thread::init();
    processor::init();
    sched::recompute_priorities(&sched::RECOMPUTE_PRIORITIES);
    arch::Current::halt()
}
