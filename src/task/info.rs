// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem;
use core::ptr;
use core::slice;
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::PAGE_SIZE;
use crate::clock::time::TimeValue;
use crate::error::Error;
use crate::thread::info::{RpcTimeValue, TimeValue64};
use crate::thread::thread::TIMER_RATE;
use crate::vm::phys_map::MachinePhysMap as _;

use super::task::Task;

/// Mach's `task_basic_info`: 30 words.
#[repr(C)]
struct BasicInfo {
    /// The stops users asked for.
    suspend_count: i32,
    base_priority: i32,
    /// How much its map maps, in bytes.
    virtual_size: usize,
    /// How much its physical map maps, in bytes.
    resident_size: usize,
    /// Its ended threads' time in user mode.
    user_time: RpcTimeValue,
    /// Its ended threads' time in the kernel.
    system_time: RpcTimeValue,
    creation_time: RpcTimeValue,
    user_time64: TimeValue64,
    system_time64: TimeValue64,
    creation_time64: TimeValue64,
}

/// Mach's `task_events_info`: 14 words.
#[repr(C)]
struct EventsInfo {
    faults: usize,
    zero_fills: usize,
    reactivations: usize,
    pageins: usize,
    cow_faults: usize,
    messages_sent: usize,
    messages_received: usize,
}

/// Mach's `task_thread_times_info`: 16 words.
#[repr(C)]
struct ThreadTimesInfo {
    /// Its live threads' time in user mode.
    user_time: RpcTimeValue,
    /// Its live threads' time in the kernel.
    system_time: RpcTimeValue,
    user_time64: TimeValue64,
    system_time64: TimeValue64,
}

impl Task {
    /// Writes its basic information into `info`, as `task_info`'s
    /// `TASK_BASIC_INFO` does; `info`'s length is the count asked for.
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
        let virtual_size = self.map.size();
        let resident_size = self.map.phys_map().resident_count() * PAGE_SIZE;
        let time = |microseconds: &AtomicU64| {
            let microseconds = microseconds.load(Ordering::Relaxed);
            TimeValue {
                seconds: i64::try_from(microseconds / u64::from(TIMER_RATE)).unwrap(),
                nanoseconds: i64::try_from(microseconds % u64::from(TIMER_RATE) * 1000).unwrap(),
            }
        };
        let members = self.members.lock();
        let user_time = time(&self.total_user_time);
        let system_time = time(&self.total_system_time);
        // As read_time_stamp; no host_set_time yet, so no offset.
        let creation_time = self.creation_time.load();
        let basic = BasicInfo {
            suspend_count: i32::try_from(members.user_stop_count).unwrap(),
            base_priority: i32::try_from(u32::from(members.priority)).unwrap(),
            virtual_size,
            resident_size,
            user_time: user_time.into(),
            system_time: system_time.into(),
            creation_time: creation_time.into(),
            user_time64: user_time.into(),
            system_time64: system_time.into(),
            creation_time64: creation_time.into(),
        };
        drop(members);
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

    /// Writes its event counts into `info`, as `task_info`'s
    /// `TASK_EVENTS_INFO` does; `info`'s length is the count asked for.
    /// Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a count short of the record.
    pub(crate) fn events_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        let count = size_of::<EventsInfo>() / size_of::<i32>();
        if info.len() < count {
            return Err(Error::InvalidArgument);
        }
        // Counted without its token, so read without it.
        let events = EventsInfo {
            faults: self.faults.load(Ordering::Relaxed),
            zero_fills: self.zero_fills.load(Ordering::Relaxed),
            // Counted only by page eviction: not yet.
            reactivations: 0,
            pageins: self.pageins.load(Ordering::Relaxed),
            cow_faults: self.cow_faults.load(Ordering::Relaxed),
            messages_sent: self.messages_sent.load(Ordering::Relaxed),
            messages_received: self.messages_received.load(Ordering::Relaxed),
        };
        // SAFETY: `count` words of integers, `repr(C)` with no padding.
        let words = unsafe { slice::from_raw_parts(ptr::from_ref(&events).cast::<i32>(), count) };
        info[..count].copy_from_slice(words);
        Ok(count)
    }

    /// Writes the times of its live threads into `info`, as `task_info`'s
    /// `TASK_THREAD_TIMES_INFO` does; `info`'s length is the count asked
    /// for. Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a count short of the record without its 64-bit
    /// times.
    pub(crate) fn thread_times_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        let count = size_of::<ThreadTimesInfo>() / size_of::<i32>();
        let short_count = mem::offset_of!(ThreadTimesInfo, user_time64) / size_of::<i32>();
        if info.len() < short_count {
            return Err(Error::InvalidArgument);
        }
        // As time_value64_add.
        let add = |sum: TimeValue, time: TimeValue| {
            TimeValue {
                seconds: sum.seconds + time.seconds,
                nanoseconds: sum.nanoseconds,
            }
            .add_nanoseconds(time.nanoseconds)
        };
        let zero = TimeValue {
            seconds: 0,
            nanoseconds: 0,
        };
        let (mut user_time, mut system_time) = (zero, zero);
        let members = self.members.lock();
        for thread in &members.threads {
            let (thread_user_time, thread_system_time) = thread.read_times();
            user_time = add(user_time, thread_user_time);
            system_time = add(system_time, thread_system_time);
        }
        drop(members);
        let times = ThreadTimesInfo {
            user_time: user_time.into(),
            system_time: system_time.into(),
            user_time64: user_time.into(),
            system_time64: system_time.into(),
        };
        // The 64-bit times only for a count holding them, as Mach's.
        let written = if info.len() >= count {
            count
        } else {
            short_count
        };
        // SAFETY: `count` words of integers, `repr(C)` with no implicit
        // padding.
        let words = unsafe { slice::from_raw_parts(ptr::from_ref(&times).cast::<i32>(), written) };
        info[..written].copy_from_slice(words);
        Ok(info.len().min(count))
    }
}
