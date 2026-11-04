// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The scheduler.

use core::sync::atomic::AtomicU32;

use intrusive_collections::LinkedList;

use super::processor::{self, ActionQueueAdapter};
use super::thread::{RunQueueAdapter, SleepQueueAdapter};
use crate::config::HZ;

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
#[expect(dead_code, reason = "recompute_priorities advances it")]
static SCHEDULER_TICK: AtomicU32 = AtomicU32::new(0);

/// The priority of system threads, and the default set's highest.
pub const SYSTEM_PRIORITY: u32 = 6;

/// The priority of user threads.
pub const USER_PRIORITY: u32 = 25;

/// The shortest quantum, in ticks: 33 context switches a second.
pub const MIN_QUANTUM: u32 = HZ / 33;

/// The scheduler's fixed-point one, which a set's load is scaled by.
pub const SCHEDULER_SCALE: u32 = 128;

bitflags::bitflags! {
    /// Scheduling policies, as Mach's interface numbers them; only those
    /// used here.
    pub struct Policies: u32 {
        /// Priorities decay with use.
        const TIMESHARE = 1;
    }
}

/// Sets up the scheduler, on the boot CPU.
pub fn init() {
    processor::bootstrap();
}
