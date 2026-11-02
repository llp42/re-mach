// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Processors and processor sets.

use super::sched::{LOAD_SCALE, MIN_QUANTUM, Policies, RUN_QUEUES, RunQueue, SYSTEM_PRIORITY};
use super::thread::RunQueueAdapter;
use crate::smp;
use crate::sync::OnceLock;

/// A processor: one per CPU, in that CPU's data.
#[expect(dead_code, reason = "the dispatcher and processor_info read them")]
pub struct Processor {
    /// What it is doing.
    state: State,
    /// Its runnable threads, by priority; only its CPU touches them, inside
    /// a critical section.
    run_queues: [RunQueue; RUN_QUEUES],
}

/// What a processor is doing; only the states used here.
enum State {
    /// Not in the system.
    Offline,
}

impl Processor {
    /// One not yet in the system.
    pub const fn new() -> Self {
        Self {
            state: State::Offline,
            run_queues: [const { RunQueue::new(RunQueueAdapter::NEW) }; RUN_QUEUES],
        }
    }
}

/// A set of processors, and the threads that run on them.
#[expect(dead_code, reason = "processor_set_info and thread_create read them")]
pub struct ProcessorSet {
    /// Whether it is in use; a destroyed set is not.
    active: bool,
    /// Whether it has no processor; threads created in it start suspended.
    empty: bool,
    /// How many references it has.
    references: u32,
    /// The highest priority its threads may take.
    max_priority: u32,
    /// The policies its threads may use.
    policies: Policies,
    /// Its processors' quantum, in ticks.
    quantum: u32,
    /// Its load, scaled by `LOAD_SCALE`.
    load: u32,
}

/// The default set, where every processor and thread starts.
#[expect(dead_code, reason = "processor_set_info and thread_create read it")]
static DEFAULT_SET: ProcessorSet = ProcessorSet {
    active: true,
    empty: true,
    references: 1,
    max_priority: SYSTEM_PRIORITY,
    policies: Policies::TIMESHARE,
    quantum: MIN_QUANTUM,
    load: LOAD_SCALE,
};

/// The boot CPU's processor.
static BOOT_PROCESSOR: OnceLock<&'static Processor> = OnceLock::new();

/// Records the boot CPU's processor, on the boot CPU.
///
/// # Panics
///
/// On a second call.
pub fn bootstrap() {
    assert!(
        BOOT_PROCESSOR.set(smp::current().processor()).is_ok(),
        "processor: bootstrapped twice"
    );
}
