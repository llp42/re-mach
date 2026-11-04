// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads.

use intrusive_collections::{LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::slab::Cache;

/// A thread; so far only what the sleep and run queues need.
pub struct Thread {
    /// Its place in the sleep queue it waits on.
    sleep_link: LinkedListAtomicLink,
    /// Its place in the run queue it waits on.
    run_link: LinkedListAtomicLink,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub SleepQueueAdapter = UnsafeRef<Thread>: Thread { sleep_link => LinkedListAtomicLink }
);

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub RunQueueAdapter = UnsafeRef<Thread>: Thread { run_link => LinkedListAtomicLink }
);

/// The cache threads come from.
static CACHE: Cache<Thread> = Cache::new("thread");

/// Sets up threads, on the boot CPU.
pub fn init() {
    CACHE.register();
}
