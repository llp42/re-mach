// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The clock: the time of day and timeouts.

use core::ptr;
use core::sync::atomic::{AtomicI32, AtomicI64, AtomicU8, AtomicU64, Ordering, fence};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::machine::{self, CpuState};
use super::sched;
use crate::arch::PAGE_SIZE;
use crate::sync::{OnceLock, Spinlock};
use crate::{smp, vm};

bitflags::bitflags! {
    /// Where a timeout stands, as Mach numbers it.
    pub struct TimeoutFlags: u8 {
        /// Set, and not cancelled.
        const ACTIVE = 2;
        /// Waiting in a wheel for its deadline.
        const PENDING = 4;
    }
}

/// A function to call once some ticks have passed.
pub struct Timeout {
    /// Its place in the wheel slot of its deadline.
    link: LinkedListAtomicLink,
    /// What it calls, with itself, at its deadline.
    #[expect(dead_code, reason = "the softclock thread calls it")]
    function: fn(&'static Self),
    /// The tick it is due at, counted from boot on its wheel's CPU.
    deadline: AtomicU64,
    /// Where it stands.
    flags: AtomicU8,
}

impl Timeout {
    /// One that calls `function`, not set.
    pub const fn new(function: fn(&'static Self)) -> Self {
        Self {
            link: LinkedListAtomicLink::new(),
            function,
            deadline: AtomicU64::new(0),
            flags: AtomicU8::new(0),
        }
    }
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

/// One CPU's timeouts, hashed by deadline into slots, each behind its own
/// spinlock so any CPU can set or cancel one.
pub struct Wheel {
    /// Each slot's timeouts, unsorted.
    slots: [Spinlock<LinkedList<TimeoutAdapter>>; WHEEL_SLOTS],
    /// How many ticks its CPU has taken since boot.
    ticks: AtomicU64,
}

impl Wheel {
    /// One with no timeout.
    pub const fn new() -> Self {
        Self {
            slots: [const { Spinlock::new(LinkedList::new(TimeoutAdapter::NEW)) }; WHEEL_SLOTS],
            ticks: AtomicU64::new(0),
        }
    }
}

/// Sets `timeout` to be due `ticks` ticks after the next one, on this CPU's
/// wheel.
///
/// # Panics
///
/// If it is waiting already: cancelling it comes with `reset_timeout`.
pub fn set_timeout(timeout: &'static Timeout, ticks: u64) {
    assert!(
        !TimeoutFlags::from_bits_truncate(timeout.flags.load(Ordering::Relaxed))
            .contains(TimeoutFlags::PENDING),
        "set_timeout: pending already"
    );
    let wheel = smp::current().timeouts();
    // Counting from the next tick leaves out the one under way.
    let deadline = wheel.ticks.load(Ordering::Relaxed) + ticks + 1;
    let mut slot = wheel.slots
        [usize::try_from(deadline % u64::try_from(WHEEL_SLOTS).unwrap()).unwrap()]
    .lock();
    timeout.deadline.store(deadline, Ordering::Relaxed);
    timeout.flags.store(
        (TimeoutFlags::ACTIVE | TimeoutFlags::PENDING).bits(),
        Ordering::Relaxed,
    );
    // SAFETY: a timeout lives while it waits in a wheel.
    slot.push_back(unsafe { UnsafeRef::from_raw(ptr::from_ref(timeout)) });
    drop(slot);
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

    /// Its value, read again while a write overlaps the read.
    fn get(&self) -> TimeValue {
        loop {
            let seconds = self.seconds.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            let nanoseconds = self.nanoseconds.load(Ordering::Relaxed);
            fence(Ordering::Acquire);
            if self.check_seconds.load(Ordering::Relaxed) == seconds {
                return TimeValue {
                    seconds,
                    nanoseconds,
                };
            }
        }
    }
}

/// The time of day, as the clock keeps it; the boot CPU writes it.
static TIME: SharedTime = SharedTime::new();

/// A time stamp: the time of day now.
pub fn stamp() -> TimeValue {
    TIME.get()
}

/// Sets the time of day, on the boot CPU.
pub fn set_time(time: TimeValue) {
    TIME.set(time);
}

/// A time whose halves are written apart, as the mapped time lays it out.
#[repr(C)]
struct MappedTimeValue {
    seconds: AtomicI64,
    nanoseconds: AtomicI64,
}

/// The time, as user tasks map it through the time device: laid out as
/// Mach's interface gives it, and written as the time of day is, a check
/// first and the seconds last.
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
    /// Writes `time` as the time of day; only one CPU writes it.
    fn set_time(&self, time: TimeValue) {
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
static MAPPED_TIME: OnceLock<&'static MappedTime> = OnceLock::new();

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
    mapped.set_time(TIME.get());
    assert!(MAPPED_TIME.set(mapped).is_ok(), "mapable_time_init: twice");
}

/// Takes a clock tick on the current CPU, `microseconds` after the one
/// before, with the thread it interrupted in user mode or not: the tick is
/// the thread's, and the CPU's in that state.
pub fn interrupt(microseconds: u32, user_mode: bool) {
    let cpu = smp::current();
    let thread = cpu.active_thread();
    if let Some(thread) = thread {
        thread.add_time(microseconds, user_mode);
    }
    let state = if user_mode {
        CpuState::User
    } else {
        CpuState::System
    };
    machine::count_tick(cpu.id(), state);
    if let Some(thread) = thread {
        sched::quantum_update(thread, 1);
    }
    let wheel = cpu.timeouts();
    wheel
        .ticks
        .store(wheel.ticks.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
}
