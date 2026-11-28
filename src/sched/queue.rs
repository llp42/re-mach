// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use tailq::TailQueue;

use crate::thread::thread::{RunQueueAdapter, SleepQueueAdapter};

/// The threads sleeping on the events that hash to one slot.
pub type SleepQueue = TailQueue<'static, SleepQueueAdapter>;

/// How many sleep queues each CPU has.
pub const SLEEP_QUEUES: usize = 1031;

/// The runnable threads of one priority.
pub type RunQueue = TailQueue<'static, RunQueueAdapter>;

/// How many run queues each processor has, one per priority.
pub const RUN_QUEUES: usize = 65;
