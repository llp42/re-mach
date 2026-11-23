// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::sync::atomic::Ordering;

use crate::error::Error;
use crate::machine::Mode;
use crate::sched::{
    self,
    policy::Policy,
    priority::{BasePriority, Priority},
};
use crate::smp;
use crate::sync::CriticalSection;

use super::info::take_delta;
use super::state::{Quantum, ThreadState};
use super::thread::Thread;

/// 5/8 to the power of a number of seconds, as two shifts: `x >> first`,
/// plus `x >> second`, or minus `x >> -second` when negative.
pub(super) const AGING: [(u32, i32); 32] = [
    (1, 1),
    (1, 3),
    (1, -3),
    (2, -7),
    (3, 5),
    (3, -5),
    (4, -8),
    (5, 7),
    (5, -7),
    (6, -10),
    (7, 10),
    (7, -9),
    (8, -11),
    (9, 12),
    (9, -11),
    (10, -13),
    (11, 14),
    (11, -13),
    (12, -15),
    (13, 17),
    (13, -15),
    (14, -17),
    (15, 19),
    (16, 18),
    (16, -19),
    (17, 22),
    (18, 20),
    (18, -20),
    (19, 26),
    (20, 22),
    (20, -22),
    (21, -27),
];

/// `usage` aged by `shifts`, one of `AGING`.
pub(super) const fn aged(usage: u32, (first, second): (u32, i32)) -> u32 {
    if second > 0 {
        (usage >> first) + (usage >> second)
    } else {
        (usage >> first) - (usage >> second.unsigned_abs())
    }
}

impl Thread {
    /// Adds `microseconds` to its time in `mode`, on the CPU it runs on.
    pub fn add_time(&self, microseconds: u32, mode: Mode) {
        let time = match mode {
            Mode::User => &self.user_time,
            Mode::Kernel => &self.system_time,
        };
        time.store(
            time.load(Ordering::Relaxed) + u64::from(microseconds),
            Ordering::Relaxed,
        );
    }

    /// Charges its use at a tick on the CPU running it, with `quantum`
    /// saying whether its quantum ran out: it catches up with the
    /// scheduler's aging, or, when timeshared, gathers the time it ran, and
    /// its priority follows once its quantum runs out or it gathered enough.
    pub fn charge_tick(&self, quantum: Quantum) {
        if self.sched_stamp.load(Ordering::Relaxed) != sched::tick::scheduler_tick() {
            self.update_priority();
        } else if self.usage_sets_priority() {
            self.take_time_delta();
            let delta = self.sched_delta.load(Ordering::Relaxed);
            if matches!(quantum, Quantum::Expired) || delta >= sched::priority::USAGE_THRESHOLD {
                let usage = self.sched_usage.load(Ordering::Relaxed);
                self.sched_usage
                    .store(usage.wrapping_add(delta), Ordering::Relaxed);
                self.sched_delta.store(0, Ordering::Relaxed);
                self.compute_my_priority();
            }
        }
    }

    /// Its next quantum: `quantum` when timeshared, else its own.
    pub const fn next_quantum(&self, quantum: u32) -> u32 {
        match self.policy {
            Policy::Timeshare => quantum,
            Policy::FixedPriority => self.fixed_quantum,
        }
    }

    /// Whether its use sets its priority: it is timeshared and not
    /// depressed.
    pub(super) fn usage_sets_priority(&self) -> bool {
        matches!(self.policy, Policy::Timeshare) && self.depressed_from.load(Ordering::Relaxed) < 0
    }

    /// Adds the CPU time it ran since its use was last charged to its
    /// deltas, the scheduler's scaled by its set's load. Use counts wrap,
    /// as Mach's do.
    fn take_time_delta(&self) {
        let delta = take_delta(&self.system_time, &self.charged_system_time)
            .wrapping_add(take_delta(&self.user_time, &self.charged_user_time));
        // SAFETY: a thread's set lives while the thread is in it.
        let set = unsafe { &*self.set.load(Ordering::Acquire) };
        let cpu_delta = self.cpu_delta.load(Ordering::Relaxed);
        self.cpu_delta
            .store(cpu_delta.wrapping_add(delta), Ordering::Relaxed);
        let sched_delta = self.sched_delta.load(Ordering::Relaxed);
        self.sched_delta.store(
            sched_delta.wrapping_add(delta.wrapping_mul(set.load())),
            Ordering::Relaxed,
        );
    }

    /// Schedules it at the priority its base priority and use give, as
    /// `compute_priority` does for a thread on no run queue.
    ///
    /// # Panics
    ///
    /// If it is not timeshared, is depressed, or is runnable: not yet.
    fn compute_priority(&self) {
        assert!(
            self.usage_sets_priority(),
            "compute_priority: fixed priority or depressed, not yet"
        );
        assert!(
            !self.state().contains(ThreadState::RUN),
            "set_pri: rem_runq, not yet"
        );
        self.compute_my_priority();
    }

    /// Schedules it at the priority its base priority and use give.
    fn compute_my_priority(&self) {
        let usage = self.sched_usage.load(Ordering::Relaxed);
        let priority = Priority::try_from(self.priority.load(Ordering::Relaxed)).unwrap();
        self.sched_pri
            .store(u32::from(priority.aged_by(usage)), Ordering::Relaxed);
    }

    /// Catches its use up with the scheduler's aging, if it missed any.
    pub(crate) fn catch_up(&self) {
        if self.sched_stamp.load(Ordering::Relaxed) != sched::tick::scheduler_tick() {
            self.update_priority();
        }
    }

    /// Whether it is stuck: its scheduling state only `RUN`, and its use last
    /// aged more than one scheduler tick ago.
    pub(crate) fn is_stuck(&self) -> bool {
        self.state() & ThreadState::SCHEDULING == ThreadState::RUN
            && sched::tick::scheduler_tick().wrapping_sub(self.sched_stamp.load(Ordering::Relaxed))
                > 1
    }

    /// Catches its use up with the scheduler's aging: each second missed
    /// ages it by 5/8, and more than 30 forget it.
    ///
    /// # Panics
    ///
    /// If it missed none.
    fn update_priority(&self) {
        let tick = sched::tick::scheduler_tick();
        let ticks = tick.wrapping_sub(self.sched_stamp.load(Ordering::Relaxed));
        assert!(ticks != 0, "update_priority: no tick missed");
        self.sched_stamp.store(tick, Ordering::Relaxed);
        self.take_time_delta();
        let (cpu_usage, sched_usage) = if ticks > 30 {
            (0, 0)
        } else {
            let shifts = AGING[usize::try_from(ticks).unwrap()];
            let cpu_usage = self.cpu_usage.load(Ordering::Relaxed);
            let sched_usage = self.sched_usage.load(Ordering::Relaxed);
            (
                aged(
                    cpu_usage.wrapping_add(self.cpu_delta.load(Ordering::Relaxed)),
                    shifts,
                ),
                aged(
                    sched_usage.wrapping_add(self.sched_delta.load(Ordering::Relaxed)),
                    shifts,
                ),
            )
        };
        self.cpu_usage.store(cpu_usage, Ordering::Relaxed);
        self.sched_usage.store(sched_usage, Ordering::Relaxed);
        self.cpu_delta.store(0, Ordering::Relaxed);
        self.sched_delta.store(0, Ordering::Relaxed);
        if self.usage_sets_priority() {
            self.compute_my_priority();
        }
    }

    /// Caps its priority at `max_priority`, no higher than its set's, its
    /// priority as `base` says, as `thread_override_max_priority` does.
    ///
    /// # Panics
    ///
    /// As `compute_priority`.
    pub(crate) fn override_max_priority(&self, max_priority: Priority, base: BasePriority) {
        let _section = CriticalSection::enter();
        // SAFETY: a thread's set lives while the thread is in it.
        let set = unsafe { &*self.set.load(Ordering::Acquire) };
        let max_priority = max_priority.weaker(set.max_priority());
        self.max_priority
            .store(u32::from(max_priority), Ordering::Relaxed);
        let priority = Priority::try_from(self.priority.load(Ordering::Relaxed)).unwrap();
        if priority.outranks(max_priority) || matches!(base, BasePriority::Set) {
            self.priority
                .store(u32::from(max_priority), Ordering::Relaxed);
        }
        self.compute_priority();
    }

    /// Sets its base priority to `priority` unless that outranks its cap, as
    /// `thread_priority` does without `set_max`.
    ///
    /// # Errors
    ///
    /// `Failure` if `priority` outranks its cap.
    ///
    /// # Panics
    ///
    /// If it is depressed: not yet; as `compute_priority`.
    pub(crate) fn try_set_priority(&self, priority: Priority) -> Result<(), Error> {
        let _section = CriticalSection::enter();
        let max_priority = Priority::try_from(self.max_priority.load(Ordering::Relaxed)).unwrap();
        if priority.outranks(max_priority) {
            return Err(Error::Failure);
        }
        assert!(
            self.depressed_from.load(Ordering::Relaxed) < 0,
            "thread_priority: a depressed thread, not yet"
        );
        self.priority.store(u32::from(priority), Ordering::Relaxed);
        if self.state().contains(ThreadState::RUN) {
            // A running one takes it the next time it is queued.
            self.compute_my_priority();
        } else {
            self.compute_priority();
        }
        Ok(())
    }

    /// Sets its base priority, while it runs.
    pub(crate) fn set_priority(&self, priority: Priority) {
        self.priority.store(u32::from(priority), Ordering::Relaxed);
    }

    /// Sets its base priority to `priority`, and with `set_max` its cap to
    /// it too, as `thread_priority` does.
    ///
    /// # Errors
    ///
    /// `Failure` if `priority` outranks its cap.
    ///
    /// # Panics
    ///
    /// If it is depressed: not yet; as `try_set_priority`.
    pub(crate) fn set_priority_and_max(
        &self,
        priority: Priority,
        set_max: bool,
    ) -> Result<(), Error> {
        self.try_set_priority(priority)?;
        if set_max {
            self.max_priority
                .store(u32::from(priority), Ordering::Relaxed);
        }
        Ok(())
    }

    /// Sets its cap to `max_priority`, its base priority raised to it if
    /// that outranks it, as `thread_max_priority` does.
    ///
    /// # Panics
    ///
    /// If it is depressed and the cap doesn't lower its base priority: not
    /// yet.
    pub(crate) fn set_max_priority(&self, max_priority: Priority) {
        let _section = CriticalSection::enter();
        self.max_priority
            .store(u32::from(max_priority), Ordering::Relaxed);
        let priority = Priority::try_from(self.priority.load(Ordering::Relaxed)).unwrap();
        if priority.outranks(max_priority) {
            self.priority
                .store(u32::from(max_priority), Ordering::Relaxed);
            if self.state().contains(ThreadState::RUN) {
                // A running one takes it the next time it is queued.
                self.compute_my_priority();
            } else {
                self.compute_priority();
            }
        } else {
            assert!(
                self.depressed_from.load(Ordering::Relaxed) < 0,
                "thread_max_priority: a depressed thread, not yet"
            );
        }
    }

    /// The priority it is scheduled at.
    pub(crate) fn scheduled_priority(&self) -> Priority {
        Priority::try_from(self.sched_pri.load(Ordering::Relaxed)).unwrap()
    }

    /// Sets the priority it is scheduled at, while on no run queue.
    pub(crate) fn set_scheduled_priority(&self, priority: Priority) {
        self.sched_pri.store(u32::from(priority), Ordering::Relaxed);
    }

    /// Its scheduling policy.
    pub(crate) const fn policy(&self) -> Policy {
        self.policy
    }
}

/// Sets the current thread's base priority to `priority`, its highest
/// raised to it if need be, and its scheduled priority from its use.
///
/// # Panics
///
/// If the thread is not timeshared, or is depressed: those come later.
pub fn set_own_priority(priority: Priority) {
    let _section = CriticalSection::enter();
    let thread = smp::current()
        .active_thread()
        .expect("thread_set_own_priority: no current thread");
    let max_priority = Priority::try_from(thread.max_priority.load(Ordering::Relaxed)).unwrap();
    if priority.outranks(max_priority) {
        thread
            .max_priority
            .store(u32::from(priority), Ordering::Relaxed);
    }
    thread
        .priority
        .store(u32::from(priority), Ordering::Relaxed);
    assert!(
        thread.usage_sets_priority(),
        "compute_priority: fixed priority or depressed, not yet"
    );
    // It runs, so it waits on no run queue.
    thread.compute_my_priority();
}
