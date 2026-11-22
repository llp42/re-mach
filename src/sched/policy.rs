// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::config::HZ;

/// The shortest quantum, in ticks: 33 context switches a second.
pub const MIN_QUANTUM: u32 = HZ / 33;

/// The scheduler's fixed-point one, which a set's load is scaled by.
pub const SCHEDULER_SCALE: u32 = 128;

/// How far `SCHEDULER_SCALE` shifts.
pub(super) const SCHEDULER_SHIFT: u32 = 7;

/// A thread's scheduling policy, one of `Policies`.
#[derive(Clone, Copy)]
pub enum Policy {
    /// Its priority decays with use.
    Timeshare,
    /// Its priority stays, and it has its own quantum.
    #[expect(dead_code, reason = "thread_policy sets it")]
    FixedPriority,
}

bitflags::bitflags! {
    /// The scheduling policies a processor set allows, as Mach's interface
    /// numbers them; only those used here.
    #[derive(Clone, Copy)]
    pub struct Policies: u32 {
        /// Priorities decay with use.
        const TIMESHARE = 1;
        /// Priorities stay, and each thread has its own quantum.
        const FIXED_PRIORITY = 2;
    }
}
