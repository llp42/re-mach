// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The clock: the time of day and timeouts.

use core::ptr;
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicI64, AtomicU8, AtomicU64, AtomicUsize, Ordering, fence,
};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::arch::{Current, Machine as _, PAGE_SIZE};
use crate::config::HZ;
use crate::machine::{self, CpuState, Mode};
use crate::processor::State;
use crate::sched::{self, Priority, WaitResult};
use crate::sync::{CriticalSection, OnceLock, Spinlock};
use crate::thread::{self, Thread};
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
    /// The last tick whose timeouts have run, moved under that tick's slot
    /// lock: by the tick while the softclock thread is not running, else by
    /// the thread.
    checked_ticks: AtomicU64,
    /// Its softclock thread, set as it is created.
    thread: OnceLock<&'static Thread>,
    /// Whether its softclock thread was woken and has not waited since.
    running: AtomicBool,
}

impl Wheel {
    /// One with no timeout.
    pub const fn new() -> Self {
        Self {
            slots: [const { Spinlock::new(LinkedList::new(TimeoutAdapter::NEW)) }; WHEEL_SLOTS],
            ticks: AtomicU64::new(0),
            checked_ticks: AtomicU64::new(0),
            thread: OnceLock::new(),
            running: AtomicBool::new(false),
        }
    }

    /// Sets its softclock thread, once.
    pub fn set_thread(&self, thread: &'static Thread) -> Result<(), &'static Thread> {
        self.thread.set(thread)
    }

    /// The slot of the timeouts due at `tick`.
    fn slot(&self, tick: u64) -> &Spinlock<LinkedList<TimeoutAdapter>> {
        &self.slots[usize::try_from(tick % u64::try_from(WHEEL_SLOTS).unwrap()).unwrap()]
    }
}

/// Sets `timeout` to be due `ticks` ticks after the next one, on this CPU's
/// wheel, cancelling it first if it is set.
///
/// # Panics
///
/// If it is waiting in a wheel already: not yet.
pub fn set_timeout(timeout: &'static Timeout, ticks: u64) {
    if TimeoutFlags::from_bits_truncate(timeout.flags.load(Ordering::Relaxed))
        .intersects(TimeoutFlags::ACTIVE | TimeoutFlags::PENDING)
    {
        reset_timeout(timeout);
    }
    // A tick waits, and the thread stays on this CPU, until the timeout is
    // in its slot.
    let _section = CriticalSection::enter();
    let wheel = smp::current().timeouts();
    // Counting from the next tick leaves out the one under way.
    let deadline = wheel.ticks.load(Ordering::Relaxed) + ticks + 1;
    let mut slot = wheel.slot(deadline).lock();
    timeout.deadline.store(deadline, Ordering::Relaxed);
    timeout.flags.store(
        (TimeoutFlags::ACTIVE | TimeoutFlags::PENDING).bits(),
        Ordering::Relaxed,
    );
    // SAFETY: a timeout lives while it waits in a wheel.
    slot.push_back(unsafe { UnsafeRef::from_raw(ptr::from_ref(timeout)) });
    drop(slot);
}

/// Cancels `timeout`, which is then not set.
///
/// # Panics
///
/// If it is waiting in a wheel: not yet.
fn reset_timeout(timeout: &Timeout) {
    let _section = CriticalSection::enter();
    assert!(
        !TimeoutFlags::from_bits_truncate(timeout.flags.load(Ordering::Relaxed))
            .contains(TimeoutFlags::PENDING),
        "reset_timeout: a pending timeout, not yet"
    );
    timeout
        .flags
        .fetch_and(!TimeoutFlags::ACTIVE.bits(), Ordering::Relaxed);
}

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

/// A time, in seconds and nanoseconds since 1970 or since boot.
#[derive(Clone, Copy)]
pub struct TimeValue {
    pub seconds: i64,
    pub nanoseconds: i64,
}

impl TimeValue {
    /// It later by `nanoseconds`, under a second.
    const fn add_nanoseconds(self, nanoseconds: i64) -> Self {
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
    fn load(&self) -> TimeValue {
        TimeValue {
            seconds: self.seconds.load(Ordering::Relaxed),
            nanoseconds: self.nanoseconds.load(Ordering::Relaxed),
        }
    }

    /// Sets it to `time`.
    pub(super) fn store(&self, time: TimeValue) {
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
struct Snapshot {
    /// The time of day.
    time: AtomicTimeValue,
    /// The time since boot, which the boot CPU's ticks advance.
    uptime: AtomicTimeValue,
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
fn snapshot() -> &'static Snapshot {
    &SNAPSHOTS[SNAPSHOT_INDEX.load(Ordering::Acquire)]
}

/// Writes `time`, `uptime` and `counter` in the next slot, then publishes
/// it, on the boot CPU.
fn publish(time: TimeValue, uptime: TimeValue, counter: u64) {
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
    fn set_uptime(&self, uptime: TimeValue) {
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
    mapped.set_time(snapshot().time.load());
    assert!(MAPPED_TIME.set(mapped).is_ok(), "mapable_time_init: twice");
}

/// Takes a clock tick on the current CPU, `microseconds` after the one
/// before, with the thread it interrupted running in `mode`: the tick is
/// the thread's, and the CPU's in that mode, or idle; it advances the
/// CPU's timeouts, and on the boot CPU the time of day and the uptime, then
/// wakes the softclock thread if it is not running and a timeout is due or
/// the wheel is behind.
pub fn interrupt(microseconds: u32, mode: Mode) {
    let cpu = smp::current();
    let thread = cpu.active_thread();
    if let Some(thread) = thread {
        thread.add_time(microseconds, mode);
    }
    let state = match mode {
        Mode::User => CpuState::User,
        Mode::Kernel if matches!(cpu.processor().state(), State::Idle) => CpuState::Idle,
        Mode::Kernel => CpuState::System,
    };
    machine::count_tick(cpu.id(), state);
    if let Some(thread) = thread {
        sched::quantum_update(thread, 1, state);
    }
    let wheel = cpu.timeouts();
    let ticks = wheel.ticks.load(Ordering::Relaxed) + 1;
    wheel.ticks.store(ticks, Ordering::Relaxed);
    // The softclock thread runs the timeouts due, and checks the ticks
    // itself while it runs; else a tick with none due, and none behind, is
    // checked at once, or the thread is woken.
    let wake = if wheel.running.load(Ordering::Relaxed) {
        false
    } else {
        let slot = wheel.slot(ticks).lock();
        let checked = slot.is_empty() && wheel.checked_ticks.load(Ordering::Relaxed) + 1 == ticks;
        if checked {
            wheel.checked_ticks.store(ticks, Ordering::Relaxed);
        } else {
            wheel.running.store(true, Ordering::Relaxed);
        }
        drop(slot);
        !checked
    };
    if cpu.id() == 0 {
        let nanoseconds = i64::from(microseconds) * 1000;
        let last = snapshot();
        let time = last.time.load().add_nanoseconds(nanoseconds);
        let uptime = last.uptime.load().add_nanoseconds(nanoseconds);
        publish(time, uptime, Current::counter_nanoseconds());
        if let Some(mapped) = MAPPED_TIME.get() {
            mapped.set_time(time);
            mapped.set_uptime(uptime);
        }
    }
    if wake {
        let thread = wheel
            .thread
            .get()
            .expect("clock_interrupt: no softclock thread");
        sched::clear_wait(thread, WaitResult::Awakened);
    }
}
