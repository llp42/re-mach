// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The interrupt thread, which delivers device interrupts to user tasks.

use crate::config::HZ;
use crate::sched::{self, Event};
use crate::thread;

/// What the interrupt thread waits on; only its address counts.
static INTR: u8 = 0;

/// The interrupt thread's work: so far no interrupt is delivered, so it
/// only waits on `INTR`, uninterruptibly and for a second at most, each
/// time it runs.
pub fn intr_thread() -> ! {
    loop {
        sched::assert_wait(Some(Event::of(&INTR)));
        thread::set_own_timeout(u64::from(HZ));
        sched::block();
    }
}
