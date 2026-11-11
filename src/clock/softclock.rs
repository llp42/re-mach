// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The softclock thread, which calls the timeouts due.

use core::sync::atomic::Ordering;

use intrusive_collections::UnsafeRef;

use super::timeout::TimeoutFlags;
use crate::sched::{self, Priority};
use crate::smp;
use crate::sync::CriticalSection;
use crate::thread;

/// A softclock thread's work, on the CPU of the processor it is bound to:
/// at the highest priority, each time it runs it calls the timeouts due on
/// its wheel up to the current tick; caught up, it marks itself not running
/// and waits with no event, in one critical section.
pub fn softclock() -> ! {
    thread::set_own_priority(Priority::HIGHEST);
    loop {
        // Bound, it stays on its wheel's CPU.
        let wheel = smp::current().timeouts();
        // While it runs, the tick leaves `checked_ticks` to it.
        while wheel.checked_ticks.load(Ordering::Relaxed) < wheel.ticks.load(Ordering::Relaxed) {
            let tick = wheel.checked_ticks.load(Ordering::Relaxed) + 1;
            // Each due timeout leaves the slot, then is called with the slot
            // unlocked, so it can set itself again.
            loop {
                let mut slot = wheel.slot(tick).lock();
                let mut cursor = slot.front_mut();
                while cursor
                    .get()
                    .is_some_and(|timeout| timeout.deadline.load(Ordering::Relaxed) != tick)
                {
                    cursor.move_next();
                }
                let Some(timeout) = cursor.remove() else {
                    wheel.checked_ticks.store(tick, Ordering::Relaxed);
                    break;
                };
                timeout
                    .flags
                    .fetch_and(!TimeoutFlags::PENDING.bits(), Ordering::Relaxed);
                drop(slot);
                // SAFETY: it was set as a `&'static Timeout`.
                let timeout = unsafe { &*UnsafeRef::into_raw(timeout) };
                (timeout.function)(timeout);
            }
        }
        // Checked again inside the section, so no tick taken since is missed.
        let section = CriticalSection::enter();
        if wheel.checked_ticks.load(Ordering::Relaxed) == wheel.ticks.load(Ordering::Relaxed) {
            wheel.running.store(false, Ordering::Relaxed);
            sched::assert_wait(None);
            sched::block();
        }
        drop(section);
    }
}
