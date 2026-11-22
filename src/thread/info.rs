// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem;
use core::ptr;
use core::slice;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::clock::time::TimeValue;
use crate::error::Error;
use crate::sched::{
    self,
    policy::{Policies, Policy},
    priority::Priority,
};
use crate::{arch, smp};

use super::machine::MachineThread as _;
use super::priority::{AGING, aged};
use super::state::{RunState, ThreadState};
use super::thread::{TIMER_RATE, Thread};

/// Mach's `rpc_time_value_t`.
#[repr(C)]
pub struct RpcTimeValue {
    seconds: i64,
    microseconds: i32,
    /// Padding to 8 bytes, zero.
    padding: i32,
}

/// Cut to microseconds, as `TIME_VALUE64_TO_TIME_VALUE`.
#[expect(
    clippy::fallible_impl_from,
    reason = "a time's nanoseconds are under a second"
)]
impl From<TimeValue> for RpcTimeValue {
    fn from(time: TimeValue) -> Self {
        Self {
            seconds: time.seconds,
            microseconds: i32::try_from(time.nanoseconds / 1000).unwrap(),
            padding: 0,
        }
    }
}

/// Mach's `time_value64_t`.
#[repr(C)]
pub struct TimeValue64 {
    seconds: i64,
    nanoseconds: i64,
}

impl From<TimeValue> for TimeValue64 {
    fn from(time: TimeValue) -> Self {
        Self {
            seconds: time.seconds,
            nanoseconds: time.nanoseconds,
        }
    }
}

/// Mach's `thread_basic_info`: 32 words.
#[repr(C)]
struct BasicInfo {
    user_time: RpcTimeValue,
    system_time: RpcTimeValue,
    /// Recent processor use, per mille.
    cpu_usage: i32,
    base_priority: i32,
    current_priority: i32,
    /// A `RunState`'s number; 0 for none.
    run_state: i32,
    /// `TH_FLAGS_*`.
    flags: i32,
    /// The stops users asked for.
    suspend_count: i32,
    /// Scheduler ticks asleep.
    sleep_time: i32,
    /// Padding to 8 bytes, zero.
    padding: i32,
    creation_time: RpcTimeValue,
    user_time64: TimeValue64,
    system_time64: TimeValue64,
    creation_time64: TimeValue64,
}

/// The time `time` gained since `charged`, which takes it now.
pub(super) fn take_delta(time: &AtomicU64, charged: &AtomicU64) -> u32 {
    let now = time.load(Ordering::Relaxed);
    let delta = now - charged.swap(now, Ordering::Relaxed);
    u32::try_from(delta).unwrap_or(u32::MAX)
}

impl Thread {
    /// Writes `thread`'s machine state of `flavor` into `state`, as
    /// `thread_get_state` does, with `thread` stopped meanwhile; `state`'s
    /// length is the count asked for. Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for no thread or the current one; as
    /// `MachineThread::read_state`.
    ///
    /// # Panics
    ///
    /// For a state the current thread reads directly: not yet. As
    /// `wait_until_stopped`, `MachineThread::read_state` and `release`.
    pub(crate) fn read_state(
        thread: Option<&'static Self>,
        flavor: i32,
        state: &mut [u32],
    ) -> Result<usize, Error> {
        let Some(thread) = thread else {
            return Err(Error::InvalidArgument);
        };
        if smp::current()
            .active_thread()
            .is_some_and(|current| ptr::eq(current, thread))
        {
            assert!(
                !arch::MachineState::reads_own_state(flavor),
                "thread_get_state: the current thread's own state, not yet"
            );
            return Err(Error::InvalidArgument);
        }
        thread.hold();
        thread.wait_until_stopped();
        // SAFETY: stopped above, and held so until the release.
        let result = unsafe { thread.machine_state.read_state(flavor, state) };
        thread.release();
        result
    }

    /// Sets `thread`'s machine state of `flavor` from `state`, as
    /// `thread_set_state` does, with `thread` stopped meanwhile unless it
    /// is the current thread and writes that state directly; `state`'s
    /// length is the count given.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for no thread, or the current one with a state it
    /// does not write directly; as `MachineThread::write_state`.
    ///
    /// # Panics
    ///
    /// As `wait_until_stopped`, `MachineThread::write_state` and `release`.
    pub(crate) fn write_state(
        thread: Option<&'static Self>,
        flavor: i32,
        state: &[u32],
    ) -> Result<(), Error> {
        let Some(thread) = thread else {
            return Err(Error::InvalidArgument);
        };
        if smp::current()
            .active_thread()
            .is_some_and(|current| ptr::eq(current, thread))
        {
            if arch::MachineState::writes_own_state(flavor) {
                // SAFETY: the current thread, with a state it writes
                // directly.
                return unsafe { thread.machine_state.write_state(flavor, state) };
            }
            return Err(Error::InvalidArgument);
        }
        thread.hold();
        thread.wait_until_stopped();
        // SAFETY: stopped above, and held so until the release.
        let result = unsafe { thread.machine_state.write_state(flavor, state) };
        thread.release();
        result
    }

    /// Writes its basic information into `info`, as `thread_info`'s
    /// `THREAD_BASIC_INFO` does; `info`'s length is the count asked for.
    /// Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a count short of the record without its 64-bit
    /// times.
    pub(crate) fn basic_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        let count = size_of::<BasicInfo>() / size_of::<i32>();
        let short_count = mem::offset_of!(BasicInfo, user_time64) / size_of::<i32>();
        if info.len() < short_count {
            return Err(Error::InvalidArgument);
        }
        let state = self.state();
        // As update_priority for a thread not running, without storing.
        let mut cpu_usage = self.cpu_usage.load(Ordering::Relaxed);
        let mut priority = self.sched_pri.load(Ordering::Relaxed);
        let ticks =
            sched::tick::scheduler_tick().wrapping_sub(self.sched_stamp.load(Ordering::Relaxed));
        if !state.contains(ThreadState::RUN) && ticks != 0 {
            let untaken = |time: &AtomicU64, charged: &AtomicU64| {
                let delta = time
                    .load(Ordering::Relaxed)
                    .saturating_sub(charged.load(Ordering::Relaxed));
                u32::try_from(delta).unwrap_or(u32::MAX)
            };
            let delta = untaken(&self.system_time, &self.charged_system_time)
                .wrapping_add(untaken(&self.user_time, &self.charged_user_time));
            // SAFETY: a thread's set lives while the thread is in it.
            let set = unsafe { &*self.set.load(Ordering::Acquire) };
            let (aged_cpu_usage, aged_sched_usage) = if ticks > 30 {
                (0, 0)
            } else {
                let shifts = AGING[usize::try_from(ticks).unwrap()];
                let cpu_usage = cpu_usage
                    .wrapping_add(self.cpu_delta.load(Ordering::Relaxed))
                    .wrapping_add(delta);
                let sched_usage = self
                    .sched_usage
                    .load(Ordering::Relaxed)
                    .wrapping_add(self.sched_delta.load(Ordering::Relaxed))
                    .wrapping_add(delta.wrapping_mul(set.load()));
                (aged(cpu_usage, shifts), aged(sched_usage, shifts))
            };
            cpu_usage = aged_cpu_usage;
            if self.usage_sets_priority() {
                let base = Priority::try_from(self.priority.load(Ordering::Relaxed)).unwrap();
                priority = u32::from(base.aged_by(aged_sched_usage));
            }
        }
        let (user_time, system_time) = self.read_times();
        let basic = BasicInfo {
            user_time: user_time.into(),
            system_time: system_time.into(),
            // Per mille (TH_USAGE_SCALE), corrected by 3/5 for the 5/8 aging.
            cpu_usage: i32::try_from(cpu_usage / (TIMER_RATE / 1000) * 3 / 5).unwrap(),
            base_priority: i32::try_from(self.priority.load(Ordering::Relaxed)).unwrap(),
            current_priority: i32::try_from(priority).unwrap(),
            run_state: RunState::try_from(state).map_or(0, i32::from),
            // TH_FLAGS_IDLE; never TH_FLAGS_SWAPPED, as stacks are kept.
            flags: if state.contains(ThreadState::IDLE) {
                2
            } else {
                0
            },
            suspend_count: i32::try_from(self.user_stop_count.load(Ordering::Relaxed)).unwrap(),
            // As after update_priority, which makes its stamp current.
            sleep_time: 0,
            padding: 0,
            // As read_time_stamp; no host_set_time yet, so no offset.
            creation_time: self.creation_time.into(),
            user_time64: user_time.into(),
            // The system time, where Mach's copies the user time.
            system_time64: system_time.into(),
            creation_time64: self.creation_time.into(),
        };
        // The 64-bit times only for the whole record's count, as Mach's.
        let written = if info.len() == count {
            count
        } else {
            short_count
        };
        // SAFETY: `count` words of integers, `repr(C)` with no implicit
        // padding.
        let words = unsafe { slice::from_raw_parts(ptr::from_ref(&basic).cast::<i32>(), written) };
        info[..written].copy_from_slice(words);
        Ok(info.len().min(count))
    }

    /// Writes its scheduling information into `info`, as `thread_info`'s
    /// `THREAD_SCHED_INFO` does; `info`'s length is the count asked for.
    /// Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a count short of the record without its last
    /// processor.
    ///
    /// # Panics
    ///
    /// For a count of the record without its last processor, a
    /// fixed-priority thread or one that has run: not yet.
    pub(crate) fn sched_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        // Mach's thread_sched_info: policy, data, base, maximum and current
        // priorities, whether depressed, the priority depressed from, last
        // processor; a count one short is allowed.
        if info.len() < 7 {
            return Err(Error::InvalidArgument);
        }
        let Some(sched) = info.first_chunk_mut::<8>() else {
            panic!("thread_info: THREAD_SCHED_INFO_COUNT - 1, not yet");
        };
        assert!(
            matches!(self.policy, Policy::Timeshare),
            "thread_info: POLICY_FIXEDPRI, not yet"
        );
        assert!(
            self.last_processor().is_none(),
            "thread_info: last_processor's slot_num, not yet"
        );
        let depressed_from = self.depressed_from.load(Ordering::Relaxed);
        *sched = [
            i32::try_from(Policies::TIMESHARE.bits()).unwrap(),
            // No data for a timeshared thread.
            0,
            i32::try_from(self.priority.load(Ordering::Relaxed)).unwrap(),
            i32::try_from(self.max_priority.load(Ordering::Relaxed)).unwrap(),
            i32::try_from(self.sched_pri.load(Ordering::Relaxed)).unwrap(),
            i32::from(depressed_from >= 0),
            depressed_from,
            // Never run, so on no processor yet.
            0,
        ];
        Ok(sched.len())
    }

    /// Its user and system times, as `thread_read_times` reads them.
    pub(crate) fn read_times(&self) -> (TimeValue, TimeValue) {
        let time = |microseconds: &AtomicU64| {
            let microseconds = microseconds.load(Ordering::Relaxed);
            TimeValue {
                seconds: i64::try_from(microseconds / u64::from(TIMER_RATE)).unwrap(),
                nanoseconds: i64::try_from(microseconds % u64::from(TIMER_RATE) * 1000).unwrap(),
            }
        };
        (time(&self.user_time), time(&self.system_time))
    }
}
