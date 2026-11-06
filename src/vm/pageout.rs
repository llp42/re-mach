// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The pageout daemon.

use super::page;
use crate::kernel::sched::{self, Event};
use crate::kernel::thread;
use crate::sync::Spinlock;

/// What the pageout daemon sleeps on until it is asked to run; it holds
/// the lock from its check of the pools until it waits.
static REQUESTED: Spinlock<()> = Spinlock::new(());

/// The pageout daemon's work, in the current thread: at the highest
/// priority, it sleeps while every pool is usable.
///
/// # Panics
///
/// If a pool is not usable: evicting pages comes later.
pub fn run() -> ! {
    thread::set_own_priority(0);
    loop {
        let requested = REQUESTED.lock();
        assert!(page::usable(), "vm_pageout_scan: eviction, not yet");
        sched::sleep(Event::of(&REQUESTED), requested);
    }
}
