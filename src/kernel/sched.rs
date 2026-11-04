// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The scheduler.

use core::sync::atomic::{AtomicU32, Ordering};

use intrusive_collections::LinkedList;

use super::ast;
use super::clock::{self, Timeout};
use super::processor::{self, ActionQueueAdapter};
use super::thread::{RunQueueAdapter, SleepQueueAdapter, Thread};
use crate::config::HZ;
use crate::smp;

/// The threads sleeping on the events that hash to one slot.
pub type SleepQueue = LinkedList<SleepQueueAdapter>;

/// How many sleep queues each CPU has.
pub const SLEEP_QUEUES: usize = 1031;

/// The runnable threads of one priority.
pub type RunQueue = LinkedList<RunQueueAdapter>;

/// How many run queues each processor has, one per priority.
pub const RUN_QUEUES: usize = 65;

/// The processors waiting for the action thread to shut them down or move
/// them to another set.
#[expect(dead_code, reason = "processor_shutdown and the action thread use it")]
static ACTION_QUEUE: LinkedList<ActionQueueAdapter> = LinkedList::new(ActionQueueAdapter::NEW);

/// How many times threads' priorities have been recomputed, once a second.
static SCHEDULER_TICK: AtomicU32 = AtomicU32::new(0);

/// The scheduler tick.
pub fn scheduler_tick() -> u32 {
    SCHEDULER_TICK.load(Ordering::Relaxed)
}

/// What recomputes threads' priorities each second.
pub static RECOMPUTE_PRIORITIES: Timeout = Timeout::new(recompute_priorities);

/// Ages threads' use once more and sets itself again for a second later,
/// on the boot CPU, then once a second.
pub fn recompute_priorities(_: &'static Timeout) {
    SCHEDULER_TICK.fetch_add(1, Ordering::Relaxed);
    clock::set_timeout(&RECOMPUTE_PRIORITIES, u64::from(HZ));
}

/// The priority of system threads, and the default set's highest.
pub const SYSTEM_PRIORITY: u32 = 6;

/// The priority of user threads.
pub const USER_PRIORITY: u32 = 25;

/// The shortest quantum, in ticks: 33 context switches a second.
pub const MIN_QUANTUM: u32 = HZ / 33;

/// The scheduler's fixed-point one, which a set's load is scaled by.
pub const SCHEDULER_SCALE: u32 = 128;

/// How far `SCHEDULER_SCALE` shifts.
pub const SCHEDULER_SHIFT: u32 = 7;

/// How far a thread's use is shifted, past `SCHEDULER_SHIFT`, to age its
/// priority.
pub const PRIORITY_SHIFT: u32 = 17;

bitflags::bitflags! {
    /// Scheduling policies, as Mach's interface numbers them; only those
    /// used here.
    #[derive(PartialEq, Eq)]
    pub struct Policies: u32 {
        /// Priorities decay with use.
        const TIMESHARE = 1;
        /// Priorities stay, and each thread has its own quantum.
        const FIXED_PRIORITY = 2;
    }
}

/// How much scaled use a thread gathers within a quantum before its
/// priority is recomputed.
pub const USAGE_THRESHOLD: u32 = 1 << (PRIORITY_SHIFT + 2 + SCHEDULER_SHIFT);

/// Charges `ticks` ticks to `thread`, running on the current CPU's
/// processor: its quantum runs down and is renewed when it runs out, its
/// priority follows its use, and the CPU checks whether it should switch.
pub fn quantum_update(thread: &'static Thread, ticks: u32) {
    let processor = smp::current().processor();
    // A processor moving between sets is in none.
    let Some(set) = processor.set() else {
        return;
    };
    let set_quantum = set.current_quantum();
    let mut quantum = if processor.queued.load(Ordering::Relaxed) == 0 {
        set_quantum
    } else {
        MIN_QUANTUM
    };
    let left = processor.quantum.load(Ordering::Relaxed) - i32::try_from(ticks).unwrap();
    processor.quantum.store(left, Ordering::Relaxed);
    if quantum != processor.last_quantum.load(Ordering::Relaxed)
        && set.processor_count.load(Ordering::Relaxed) > 1
    {
        processor.last_quantum.store(quantum, Ordering::Relaxed);
        quantum = set.adjust_quantum(quantum);
    }
    if left <= 0 {
        thread.charge_tick(true);
        processor.first_quantum.store(false, Ordering::Relaxed);
        let next = i32::try_from(thread.next_quantum(quantum)).unwrap();
        processor.quantum.store(left + next, Ordering::Relaxed);
    } else {
        thread.charge_tick(false);
    }
    ast::check(thread);
}

/// Sets up the scheduler, on the boot CPU.
pub fn init() {
    processor::bootstrap();
}
