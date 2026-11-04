// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The clock: the time of day and timeouts.

use core::sync::atomic::{AtomicI64, Ordering, fence};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

/// A function to call once some ticks have passed; so far only its place
/// in a wheel.
pub struct Timeout {
    /// Its place in the wheel slot of its deadline.
    link: LinkedListAtomicLink,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub TimeoutAdapter = UnsafeRef<Timeout>: Timeout { link => LinkedListAtomicLink }
);

/// How many slots a wheel has; a timeout waits in the slot of its deadline
/// modulo this.
pub const WHEEL_SLOTS: usize = 256;

/// One CPU's timeouts, hashed by deadline into slots.
pub struct Wheel {
    /// Each slot's timeouts, unsorted.
    #[expect(dead_code, reason = "set_timeout and the softclock thread use them")]
    slots: [LinkedList<TimeoutAdapter>; WHEEL_SLOTS],
}

impl Wheel {
    /// One with no timeout.
    pub const fn new() -> Self {
        Self {
            slots: [const { LinkedList::new(TimeoutAdapter::NEW) }; WHEEL_SLOTS],
        }
    }
}

/// A time, in seconds and nanoseconds since 1970 or since boot.
#[derive(Clone, Copy)]
pub struct TimeValue {
    pub seconds: i64,
    pub nanoseconds: i64,
}

/// A time one CPU writes and any reads without a lock: the seconds are
/// written before and after the rest, so a reader that sees both the same
/// read a whole time.
struct SharedTime {
    /// The seconds, written first.
    check_seconds: AtomicI64,
    nanoseconds: AtomicI64,
    /// The seconds, written last.
    seconds: AtomicI64,
}

impl SharedTime {
    /// Zero.
    const fn new() -> Self {
        Self {
            check_seconds: AtomicI64::new(0),
            nanoseconds: AtomicI64::new(0),
            seconds: AtomicI64::new(0),
        }
    }

    /// Sets it to `time`; only one CPU writes it.
    fn set(&self, time: TimeValue) {
        self.check_seconds.store(time.seconds, Ordering::Relaxed);
        fence(Ordering::Release);
        self.nanoseconds.store(time.nanoseconds, Ordering::Relaxed);
        fence(Ordering::Release);
        self.seconds.store(time.seconds, Ordering::Relaxed);
    }
}

/// The time of day, as the clock keeps it; the boot CPU writes it.
static TIME: SharedTime = SharedTime::new();

/// Sets the time of day, on the boot CPU.
pub fn set_time(time: TimeValue) {
    TIME.set(time);
}
