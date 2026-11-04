// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Processors and processor sets.

use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32};

use intrusive_collections::{LinkedListAtomicLink, intrusive_adapter};

use super::sched::{MIN_QUANTUM, Policies, RUN_QUEUES, RunQueue, SCHEDULER_SCALE, SYSTEM_PRIORITY};
use super::thread::RunQueueAdapter;
use crate::ipc::port::Port;
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
    /// Its place in the action queue.
    action_link: LinkedListAtomicLink,
    /// Its port, for controlling it.
    pub(super) port: AtomicPtr<Port>,
    /// Its name port, for information about it.
    pub(super) name_port: AtomicPtr<Port>,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub ActionQueueAdapter = &'static Processor: Processor { action_link => LinkedListAtomicLink }
);

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
            action_link: LinkedListAtomicLink::new(),
            port: AtomicPtr::new(ptr::null_mut()),
            name_port: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

/// A set of processors, and the threads that run on them.
#[expect(dead_code, reason = "processor_set_info and thread_create read them")]
pub struct ProcessorSet {
    /// Whether it is in use; a destroyed set is not.
    pub(super) active: bool,
    /// Whether it has no processor; threads created in it start suspended.
    empty: bool,
    /// How many references it has.
    pub(super) references: AtomicU32,
    /// The highest priority its threads may take.
    max_priority: u32,
    /// The policies its threads may use.
    policies: Policies,
    /// Its processors' quantum, in ticks.
    quantum: u32,
    /// Its load, scaled by `SCHEDULER_SCALE`.
    load: u32,
    /// Its port, for controlling it.
    pub(super) port: AtomicPtr<Port>,
    /// Its name port, for information about it.
    pub(super) name_port: AtomicPtr<Port>,
}

/// The default set, where every processor and thread starts.
pub(super) static DEFAULT_SET: ProcessorSet = ProcessorSet {
    active: true,
    empty: true,
    references: AtomicU32::new(1),
    max_priority: SYSTEM_PRIORITY,
    policies: Policies::TIMESHARE,
    quantum: MIN_QUANTUM,
    load: SCHEDULER_SCALE,
    port: AtomicPtr::new(ptr::null_mut()),
    name_port: AtomicPtr::new(ptr::null_mut()),
};

/// The boot CPU's processor.
pub(super) static BOOT_PROCESSOR: OnceLock<&'static Processor> = OnceLock::new();

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
