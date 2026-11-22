// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::sync::atomic::{AtomicU32, AtomicUsize, Ordering};

use crate::ast;
use crate::clock;
use crate::clock::timeout::Timeout;
use crate::config::HZ;
use crate::machine::CpuState;
use crate::processor;
use crate::sched::event::Event;
use crate::sched::load::compute_mach_factor;
use crate::sched::policy::MIN_QUANTUM;
use crate::sched::switch::{block, do_thread_scan};
use crate::sched::wait::{WaitResult, assert_wait, clear_wait};
use crate::smp;
use crate::sync::OnceLock;
use crate::thread::state::Quantum;
use crate::thread::thread::Thread;

/// The processors waiting for the action thread to shut them down or move
/// them to another set, a bit for each CPU: set with Release, taken with
/// Acquire.
static ACTION: AtomicUsize = AtomicUsize::new(0);

/// How many times threads' priorities have been recomputed, once a second.
static SCHEDULER_TICK: AtomicU32 = AtomicU32::new(0);

/// The scheduler tick.
pub fn scheduler_tick() -> u32 {
    SCHEDULER_TICK.load(Ordering::Relaxed)
}

/// The scheduler thread, set as it starts.
static SCHED_THREAD: OnceLock<&'static Thread> = OnceLock::new();

/// What recomputes threads' priorities each second.
pub static RECOMPUTE_PRIORITIES: Timeout = Timeout::new(recompute_priorities);

/// Ages threads' use once more and sets itself again for a second later,
/// on the boot CPU, then once a second; it wakes the scheduler thread,
/// once that has started.
pub fn recompute_priorities(_: &'static Timeout) {
    SCHEDULER_TICK.fetch_add(1, Ordering::Relaxed);
    clock::timeout::set_timeout(&RECOMPUTE_PRIORITIES, u64::from(HZ));
    if let Some(&thread) = SCHED_THREAD.get() {
        // Its home CPU, CPU 0 (the default set's), runs this timeout.
        clear_wait(thread, WaitResult::Awakened);
    }
}

/// Charges `ticks` ticks the current CPU spent in `state` to `thread`,
/// running on its processor: unless idle, its quantum runs down and is
/// renewed when it runs out, its priority follows its use, and the CPU
/// checks whether it should switch.
pub fn quantum_update(thread: &'static Thread, ticks: u32, state: CpuState) {
    let processor = smp::current().processor();
    // A processor moving between sets is in none.
    let Some(set) = processor.set() else {
        return;
    };
    let set_quantum = set.current_quantum();
    let mut quantum = if processor.bound_queued() == 0 {
        set_quantum
    } else {
        MIN_QUANTUM
    };
    if matches!(state, CpuState::Idle) {
        return;
    }
    let left = processor.charge_quantum(ticks);
    if quantum != processor.last_quantum() && set.processor_count() > 1 {
        processor.set_last_quantum(quantum);
        quantum = set.adjust_quantum(quantum);
    }
    if left <= 0 {
        thread.charge_tick(Quantum::Expired);
        processor.end_first_quantum();
        let next = i32::try_from(thread.next_quantum(quantum)).unwrap();
        processor.set_quantum(left + next);
    } else {
        thread.charge_tick(Quantum::Left);
    }
    ast::check(thread);
}

/// The scheduler thread's work: it records itself as the scheduler
/// thread, then waits with no event, uninterruptibly, for
/// `recompute_priorities` to wake it; woken, it computes the Mach factor
/// and, at an odd scheduler tick, scans for stuck threads, then waits
/// again.
///
/// # Panics
///
/// If a scheduler thread ran already.
pub fn sched_thread() -> ! {
    let thread = smp::current()
        .active_thread()
        .expect("sched_thread: no current thread");
    assert!(SCHED_THREAD.set(thread).is_ok(), "sched_thread: twice");
    loop {
        assert_wait(None);
        block();
        compute_mach_factor();
        if scheduler_tick() % 2 == 1 {
            do_thread_scan();
        }
    }
}

/// The action thread's work: each time it runs it waits on `ACTION`,
/// uninterruptibly, then takes the processors there, so far none, and
/// blocks.
///
/// # Panics
///
/// If a processor waits there: not yet.
pub fn action_thread() -> ! {
    loop {
        // Before the check, so a later wake is not lost.
        assert_wait(Some(Event::of(&ACTION)));
        assert!(
            ACTION.swap(0, Ordering::Acquire) == 0,
            "processor_doaction: not yet"
        );
        block();
    }
}

/// Sets up the scheduler, on the boot CPU.
pub fn init() {
    processor::bootstrap();
}
