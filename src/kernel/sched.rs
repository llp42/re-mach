// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The scheduler.

use intrusive_collections::LinkedList;

use super::thread::SleepQueueAdapter;
use crate::config::HZ;

/// The threads sleeping on the events that hash to one slot.
pub type SleepQueue = LinkedList<SleepQueueAdapter>;

/// How many sleep queues each CPU has.
pub const SLEEP_QUEUES: usize = 1031;

/// The priority of system threads, and the default set's highest.
pub const SYSTEM_PRIORITY: u32 = 6;

/// The shortest quantum, in ticks: 33 context switches a second.
pub const MIN_QUANTUM: u32 = HZ / 33;

/// The fixed-point one of a set's load.
pub const LOAD_SCALE: u32 = 128;

bitflags::bitflags! {
    /// Scheduling policies, as Mach's interface numbers them; only those
    /// used here.
    pub struct Policies: u32 {
        /// Priorities decay with use.
        const TIMESHARE = 1;
    }
}
