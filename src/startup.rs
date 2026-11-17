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
/// bootstrap tasks; so far only an idle thread for each processor, then a
/// softclock thread for each, the reaper, the scheduler thread, the
/// interrupt thread, the action thread, the start of the other CPUs, the
/// device service, the kernel task's creation time, the start of the
/// bootstrap and the release of the other CPUs, then it becomes the
/// pageout daemon.
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
    for cpu in (0..MAX_CPUS).filter(|&cpu| machine::has_processor(cpu)) {
        let thread = Thread::create(kernel_task)
            .unwrap_or_else(|error| panic!("start_kernel_threads: thread_create: {error:?}"));
        thread.set_name(format_args!("softclock/{cpu}"));
        thread.bind(smp::of(cpu).processor());
        thread.start(clock::softclock::softclock);
        assert!(
            smp::of(cpu).timeouts().set_thread(thread).is_ok(),
            "start_kernel_threads: a softclock thread already"
        );
        thread
            .resume()
            .unwrap_or_else(|error| panic!("start_kernel_threads: thread_resume: {error:?}"));
    }
    thread::kernel_thread(kernel_task, thread::reaper_thread)
        .unwrap_or_else(|error| panic!("start_kernel_threads: kernel_thread: {error:?}"));
    thread::kernel_thread(kernel_task, sched::sched_thread)
        .unwrap_or_else(|error| panic!("start_kernel_threads: kernel_thread: {error:?}"));
    thread::kernel_thread(kernel_task, device::interrupt::intr_thread)
        .unwrap_or_else(|error| panic!("start_kernel_threads: kernel_thread: {error:?}"));
    thread::kernel_thread(kernel_task, sched::action_thread)
        .unwrap_or_else(|error| panic!("start_kernel_threads: kernel_thread: {error:?}"));
    arch::Current::start_other_cpus();
    device::create_service();
    kernel_task.creation_time.store(clock::time::stamp());
    bootstrap::create();
    arch::Current::release_other_cpus();
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
    // The boot CPU uses the kernel's page tables; SeqCst, though the other
    // CPUs start only later.
    smp::STARTED.insert(smp::current().id());
    arch::Current::init();
    clock::mapped_time::map_time();
    machine::init_info();
    task::init();
    thread::init();
    processor::init();
    sched::recompute_priorities(&sched::RECOMPUTE_PRIORITIES);
    sched::compute_mach_factor();
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
    launch_first_thread(Some(startup))
}

/// Brings the current CPU up and starts its first thread on it: `thread`
/// on the boot CPU, else the first its processor's run queues give.
///
/// # Panics
///
/// With no thread given and none queued.
pub fn launch_first_thread(thread: Option<&'static Thread>) -> ! {
    let cpu = smp::current();
    machine::cpu_up(cpu.id());
    let section = CriticalSection::enter();
    let first = thread
        .or_else(|| cpu.processor().dequeue())
        .expect("cpu_launch_first_thread");
    arch::Current::activate_kernel_tables();
    first.run_here();
    // The other CPUs' clocks started at their park.
    if thread.is_some() {
        arch::Current::start_clock();
    }
    first.load(section)
}
