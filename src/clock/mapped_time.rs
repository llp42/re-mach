// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The mapped time: the page user tasks read the time from.

use core::sync::atomic::{AtomicI32, AtomicI64, Ordering, fence};

use super::time::{TimeValue, snapshot};
use crate::arch::PAGE_SIZE;
use crate::sync::OnceLock;
use crate::vm;

/// A time whose halves are written apart, as the mapped time lays it out.
#[repr(C)]
struct MappedTimeValue {
    seconds: AtomicI64,
    nanoseconds: AtomicI64,
}

/// The time, as user tasks map it through the time device: laid out as
/// Mach's interface gives it, and written a check first and the seconds
/// last.
#[repr(C)]
pub struct MappedTime {
    /// The time of day's seconds, in 32 bits.
    seconds: AtomicI32,
    microseconds: AtomicI32,
    check_seconds: AtomicI32,
    time_value: MappedTimeValue,
    check_seconds64: AtomicI64,
    /// The uptime, which the clock's ticks advance.
    uptime_value: MappedTimeValue,
    check_upseconds64: AtomicI64,
}

impl MappedTime {
    /// Writes `uptime` as the time since boot; only one CPU writes it.
    pub(super) fn set_uptime(&self, uptime: TimeValue) {
        self.check_upseconds64
            .store(uptime.seconds, Ordering::Relaxed);
        fence(Ordering::Release);
        self.uptime_value
            .nanoseconds
            .store(uptime.nanoseconds, Ordering::Relaxed);
        fence(Ordering::Release);
        self.uptime_value
            .seconds
            .store(uptime.seconds, Ordering::Relaxed);
    }

    /// Writes `time` as the time of day; only one CPU writes it.
    pub(super) fn set_time(&self, time: TimeValue) {
        // The 32-bit fields keep the low half, as Mach's do.
        let low = |value: i64| i32::from_le_bytes(value.to_le_bytes()[..4].try_into().unwrap());
        self.check_seconds
            .store(low(time.seconds), Ordering::Relaxed);
        self.check_seconds64.store(time.seconds, Ordering::Relaxed);
        fence(Ordering::Release);
        self.microseconds
            .store(low(time.nanoseconds / 1000), Ordering::Relaxed);
        self.time_value
            .nanoseconds
            .store(time.nanoseconds, Ordering::Relaxed);
        fence(Ordering::Release);
        self.seconds.store(low(time.seconds), Ordering::Relaxed);
        self.time_value
            .seconds
            .store(time.seconds, Ordering::Relaxed);
    }
}

/// The mapped time, once `map_time` takes its page.
pub(super) static MAPPED_TIME: OnceLock<&'static MappedTime> = OnceLock::new();

/// Takes the page user tasks map the time from, wired, and writes the time
/// of day in it, on the boot CPU.
///
/// # Panics
///
/// If no page can be taken, or on a second call.
pub fn map_time() {
    const { assert!(size_of::<MappedTime>() <= PAGE_SIZE) };
    let page = vm::kernel::allocate_wired(PAGE_SIZE)
        .unwrap_or_else(|error| panic!("mapable_time_init: {error:?}"));
    // SAFETY: a zeroed page, the kernel's for good and mapped at `page`,
    // laid out as zeros of `MappedTime`'s atomics.
    let mapped = unsafe { page.cast::<MappedTime>().as_ref() };
    mapped.set_time(snapshot().time.load());
    assert!(MAPPED_TIME.set(mapped).is_ok(), "mapable_time_init: twice");
}
