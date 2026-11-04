// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Machine-independent startup.

use super::{clock, machine, processor, radix_tree, sched, task, thread};
use crate::arch::{self, Machine as _};
use crate::{ipc, vm};

/// The startup thread's work: starting the other kernel threads and the
/// bootstrap tasks; so far it only stops its CPU.
fn start_kernel_threads() -> ! {
    arch::Current::halt()
}

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
    let kernel_task = task::KERNEL_TASK.get().expect("startup: no kernel task");
    let startup = thread::create(kernel_task)
        .unwrap_or_else(|error| panic!("startup: thread_create: {error:?}"));
    startup.set_name("startup");
    startup.start(start_kernel_threads);
    arch::Current::halt()
}
