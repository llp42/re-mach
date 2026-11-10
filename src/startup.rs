// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Machine-independent startup.

use crate::arch::{self, Machine as _};
use crate::collections::radix_tree;
use crate::config::MAX_CPUS;
use crate::sync::CriticalSection;
use crate::thread::{self, Thread};
use crate::{bootstrap, clock, machine, processor, sched, task};
use crate::{device, ipc, smp, vm};

/// The startup thread's work: starting the other kernel threads and the
/// bootstrap tasks; so far only an idle thread for each processor, the
/// device service, the kernel task's creation time and the start of the
/// bootstrap, then it becomes the pageout daemon.
fn start_kernel_threads() -> ! {
    let kernel_task = task::KERNEL_TASK
        .get()
        .expect("start_kernel_threads: no kernel task");
    for cpu in (0..MAX_CPUS).filter(|&cpu| machine::has_processor(cpu)) {
        let thread = Thread::create(kernel_task)
            .unwrap_or_else(|error| panic!("start_kernel_threads: thread_create: {error:?}"));
        thread.set_name(format_args!("idle/{cpu}"));
        thread.bind(smp::of(cpu).processor());
        thread.start(sched::idle_thread);
        thread
            .resume()
            .unwrap_or_else(|error| panic!("start_kernel_threads: thread_resume: {error:?}"));
    }
    device::create_service();
    kernel_task.creation_time.store(clock::stamp());
    bootstrap::create();
    smp::current()
        .active_thread()
        .expect("start_kernel_threads: no current thread")
        .set_name(format_args!("pageout"));
    vm::pageout::run()
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
    let startup = Thread::create(kernel_task)
        .unwrap_or_else(|error| panic!("startup: thread_create: {error:?}"));
    startup.set_name(format_args!("startup"));
    startup.start(start_kernel_threads);
    // It looks as if it runs already, so resuming it puts it on no run
    // queue.
    startup.mark_running();
    startup
        .resume()
        .unwrap_or_else(|error| panic!("startup: thread_resume: {error:?}"));
    launch_first_thread(startup)
}

/// Brings the current CPU up and starts `thread` on it, its first.
fn launch_first_thread(thread: &'static Thread) -> ! {
    machine::cpu_up(smp::current().id());
    let section = CriticalSection::enter();
    thread.run_here();
    arch::Current::start_clock();
    thread.load(section)
}
