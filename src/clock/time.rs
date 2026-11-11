// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The time of day and the uptime.

use core::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};

use crate::arch::{Current, Machine as _};
use crate::config::HZ;

/// A time, in seconds and nanoseconds since 1970 or since boot.
#[derive(Clone, Copy)]
pub struct TimeValue {
    pub seconds: i64,
    pub nanoseconds: i64,
}

impl TimeValue {
    /// It later by `nanoseconds`, under a second.
    pub(super) const fn add_nanoseconds(self, nanoseconds: i64) -> Self {
        let nanoseconds = self.nanoseconds + nanoseconds;
        if nanoseconds >= NANOSECONDS_PER_SECOND {
            Self {
                seconds: self.seconds + 1,
                nanoseconds: nanoseconds - NANOSECONDS_PER_SECOND,
            }
        } else {
            Self {
                seconds: self.seconds,
                nanoseconds,
            }
        }
    }
}

/// Nanoseconds in a second.
const NANOSECONDS_PER_SECOND: i64 = 1_000_000_000;

/// A time in two atomics, read and written apart.
pub struct AtomicTimeValue {
    seconds: AtomicI64,
    nanoseconds: AtomicI64,
}

impl AtomicTimeValue {
    /// Zero.
    const fn new() -> Self {
        Self {
            seconds: AtomicI64::new(0),
            nanoseconds: AtomicI64::new(0),
        }
    }

    /// Its value.
    pub(super) fn load(&self) -> TimeValue {
        TimeValue {
            seconds: self.seconds.load(Ordering::Relaxed),
            nanoseconds: self.nanoseconds.load(Ordering::Relaxed),
        }
    }

    /// Sets it to `time`.
    pub fn store(&self, time: TimeValue) {
        self.seconds.store(time.seconds, Ordering::Relaxed);
        self.nanoseconds.store(time.nanoseconds, Ordering::Relaxed);
    }
}

impl From<TimeValue> for AtomicTimeValue {
    fn from(time: TimeValue) -> Self {
        Self {
            seconds: AtomicI64::new(time.seconds),
            nanoseconds: AtomicI64::new(time.nanoseconds),
        }
    }
}

/// The clock at one moment, written whole on the boot CPU, then published.
pub(super) struct Snapshot {
    /// The time of day.
    pub(super) time: AtomicTimeValue,
    /// The time since boot, which the boot CPU's ticks advance.
    pub(super) uptime: AtomicTimeValue,
    /// The time counter at the boot CPU's last tick.
    counter: AtomicU64,
}

impl Snapshot {
    /// Zero.
    const fn new() -> Self {
        Self {
            time: AtomicTimeValue::new(),
            uptime: AtomicTimeValue::new(),
            counter: AtomicU64::new(0),
        }
    }
}

/// The clock's snapshots, a ring only the boot CPU writes; a reader stalled
/// across 16 publications can see its slot rewritten.
static SNAPSHOTS: [Snapshot; 16] = [const { Snapshot::new() }; _];

/// The slot of the published snapshot, the one readers take.
static SNAPSHOT_INDEX: AtomicUsize = AtomicUsize::new(0);

/// The published snapshot.
pub(super) fn snapshot() -> &'static Snapshot {
    &SNAPSHOTS[SNAPSHOT_INDEX.load(Ordering::Acquire)]
}

/// Writes `time`, `uptime` and `counter` in the next slot, then publishes
/// it, on the boot CPU.
pub(super) fn publish(time: TimeValue, uptime: TimeValue, counter: u64) {
    let index = (SNAPSHOT_INDEX.load(Ordering::Relaxed) + 1) % SNAPSHOTS.len();
    let next = &SNAPSHOTS[index];
    next.time.store(time);
    next.uptime.store(uptime);
    next.counter.store(counter, Ordering::Relaxed);
    SNAPSHOT_INDEX.store(index, Ordering::Release);
}

/// A time stamp: the time of day now, with the counter's change since the
/// boot CPU's last tick, kept under a tick.
pub fn stamp() -> TimeValue {
    let last = snapshot();
    let since = Current::counter_nanoseconds().saturating_sub(last.counter.load(Ordering::Relaxed));
    let since = i64::try_from(since).unwrap_or(i64::MAX);
    last.time
        .load()
        .add_nanoseconds(since.min(NANOSECONDS_PER_SECOND / i64::from(HZ) - 1))
}

/// Sets the time of day, on the boot CPU.
pub fn set_time(time: TimeValue) {
    let last = snapshot();
    publish(
        time,
        last.uptime.load(),
        last.counter.load(Ordering::Relaxed),
    );
}
