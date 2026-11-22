// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use intrusive_collections::LinkedList;

use crate::task::task::SetTaskAdapter;
use crate::thread::thread::SetThreadAdapter;

/// A set's tasks and threads, and how many of each.
pub struct Members {
    /// The tasks, in the order the set took them.
    pub(super) tasks: LinkedList<SetTaskAdapter>,
    /// How many tasks there are.
    pub(super) task_count: u32,
    /// The threads, in the order the set took them.
    pub(super) threads: LinkedList<SetThreadAdapter>,
    /// How many threads there are.
    pub(super) thread_count: u32,
}
