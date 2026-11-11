// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Timeouts, in a wheel for each CPU.

use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::smp;
use crate::sync::{CriticalSection, OnceLock, Spinlock};
use crate::thread::Thread;

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
    pub(super) function: fn(&'static Self),
    /// The tick it is due at, counted from boot on its wheel's CPU.
    pub(super) deadline: AtomicU64,
    /// Where it stands.
    pub(super) flags: AtomicU8,
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
    pub(super) ticks: AtomicU64,
    /// The last tick whose timeouts have run, moved under that tick's slot
    /// lock: by the tick while the softclock thread is not running, else by
    /// the thread.
    pub(super) checked_ticks: AtomicU64,
    /// Its softclock thread, set as it is created.
    pub(super) thread: OnceLock<&'static Thread>,
    /// Whether its softclock thread was asked to run and has not waited
    /// since.
    pub(super) running: AtomicBool,
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
    pub(super) fn slot(&self, tick: u64) -> &Spinlock<LinkedList<TimeoutAdapter>> {
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
