// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::error::Error;
use crate::sched::policy::SCHEDULER_SHIFT;

/// A thread's, task's or set's priority: Mach counts 0 as the highest.
#[derive(Clone, Copy)]
pub struct Priority(u32);

impl Priority {
    /// The highest, the first run queue's.
    pub const HIGHEST: Self = Self(0);

    /// The lowest, the last run queue's.
    pub const LOWEST: Self = Self(64);

    /// Whether it is higher than `other`.
    pub const fn outranks(self, other: Self) -> bool {
        self.0 < other.0
    }

    /// The lower of it and `other`.
    pub fn weaker(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }

    /// Its run queue's index.
    pub fn index(self) -> usize {
        usize::try_from(self.0).unwrap()
    }

    /// The priority a thread of this base priority is scheduled at after
    /// `usage`, within the run queues.
    pub fn aged_by(self, usage: u32) -> Self {
        Self((self.0 + (usage >> (PRIORITY_SHIFT + SCHEDULER_SHIFT))).min(Self::LOWEST.0))
    }
}

impl From<Priority> for u32 {
    fn from(priority: Priority) -> Self {
        priority.0
    }
}

/// Fails with `InvalidArgument` past the lowest.
impl TryFrom<u32> for Priority {
    type Error = Error;

    fn try_from(priority: u32) -> Result<Self, Error> {
        if priority <= Self::LOWEST.0 {
            Ok(Self(priority))
        } else {
            Err(Error::InvalidArgument)
        }
    }
}

/// What a new cap does to a base priority.
#[derive(Clone, Copy)]
pub enum BasePriority {
    /// Sets it to the cap.
    Set,
    /// Lowers it to the cap if it outranks the cap.
    Capped,
}

/// The priority of system threads, and the default set's highest.
pub const SYSTEM_PRIORITY: Priority = Priority(6);

/// The priority of user threads.
pub const USER_PRIORITY: Priority = Priority(25);

/// How far a thread's use is shifted, past `SCHEDULER_SHIFT`, to age its
/// priority.
const PRIORITY_SHIFT: u32 = 17;

/// How much scaled use a thread gathers within a quantum before its
/// priority is recomputed.
pub const USAGE_THRESHOLD: u32 = 1 << (PRIORITY_SHIFT + 2 + SCHEDULER_SHIFT);
