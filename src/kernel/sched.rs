// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The scheduler.

use intrusive_collections::LinkedList;

use super::thread::SleepQueueAdapter;

/// The threads sleeping on the events that hash to one slot.
pub type SleepQueue = LinkedList<SleepQueueAdapter>;

/// How many sleep queues each CPU has.
pub const SLEEP_QUEUES: usize = 1031;
