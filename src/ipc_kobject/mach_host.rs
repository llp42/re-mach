// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The host calls' server routines, as MIG makes them from `mach_host`'s
//! definitions; only those written so far.

use super::mig;
use super::mig::boolean::Boolean;
use super::mig::string_char::StringC;
use crate::error::Error;
use crate::host::{self, Host};
use crate::ipc::MessageType;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::sched::priority::{BasePriority, Priority};
use crate::task::ports::Threads;
use crate::task::task::Task;
use crate::{clock, machine};

mig::subsystem! {
    base 2600;
    22 task_priority(task = Port::to_task, priority: i32, change_threads: Boolean);
    34 host_get_time(host = Port::as_host) -> (current_time: RpcTimeValue);
    35 host_reboot(host = Port::as_privileged_host, options: i32);
    38 host_info(host = Port::as_host, flavor: i32) -> (info: [i32; ..1024], CountInOut);
    43 host_get_time64(host = Port::as_host) -> (current_time: TimeValue64);
    46 host_get_kernel_version(host = Port::as_host) -> (kernel_version: StringC<512>);
    47 host_get_uptime64(host = Port::as_host) -> (uptime: TimeValue64);
    49 task_max_priority(
        host: Port,
        task = Port::to_task,
        max_priority: i32,
        set_priority: Boolean,
        change_threads: Boolean
    );
}

/// Mach's `rpc_time_value_t`, as MIG carries it.
#[derive(Clone, Copy)]
#[repr(C)]
struct RpcTimeValue {
    /// Its seconds.
    seconds: i64,
    /// Its microseconds.
    microseconds: i32,
    /// Padding to 8 bytes.
    padding: [u8; 4],
}

// SAFETY: `repr(C)`, integers only, no padding.
unsafe impl mig::argument::Inline for RpcTimeValue {
    const WORD: u64 = mig::argument::type_word(MessageType::Integer64, 64, 2);
}

/// Mach's `time_value64_t`, as MIG carries it.
#[derive(Clone, Copy)]
#[repr(C)]
struct TimeValue64 {
    /// Its seconds.
    seconds: i64,
    /// Its nanoseconds.
    nanoseconds: i64,
}

// SAFETY: `repr(C)`, integers only, no padding.
unsafe impl mig::argument::Inline for TimeValue64 {
    const WORD: u64 = mig::argument::type_word(MessageType::Integer64, 64, 2);
}

/// Serves `task_priority`: sets `task`'s priority to `priority`, its
/// threads' too for `change_threads`.
///
/// # Errors
///
/// `InvalidArgument` for no task or a priority past the lowest; as
/// `Task::set_priority`.
///
/// # Panics
///
/// As `Task::set_priority`.
fn task_priority(
    task: Option<&Reference<Task>>,
    priority: i32,
    change_threads: Boolean,
) -> Result<(), Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    let priority = u32::try_from(priority)
        .map_err(|_| Error::InvalidArgument)
        .and_then(Priority::try_from)?;
    let threads = if bool::from(change_threads) {
        Threads::Changed
    } else {
        Threads::Kept
    };
    task.set_priority(priority, threads)
}

/// Serves `host_get_time`: the time of day for `host`.
///
/// # Panics
///
/// As `clock::time::host_time`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn host_get_time(host: Option<&&Host>) -> Result<RpcTimeValue, Error> {
    let time = clock::time::host_time(host.copied());
    Ok(RpcTimeValue {
        seconds: time.seconds(),
        microseconds: time.microseconds(),
        padding: [0; 4],
    })
}

/// Serves `host_reboot`; it doesn't return.
///
/// # Panics
///
/// As `machine::host_reboot`.
fn host_reboot(host: Option<&&Host>, options: i32) -> Result<(), Error> {
    machine::host_reboot(host.copied(), options)
}

/// Serves `host_info`: up to `info.len()` integers of `flavor` for `host`.
///
/// # Panics
///
/// As `host::info`.
fn host_info(host: Option<&&Host>, flavor: i32, info: &mut [i32]) -> Result<usize, Error> {
    host::info(host.copied(), flavor, info)
}

/// Serves `host_get_time64`: the time of day for `host`.
///
/// # Panics
///
/// As `clock::time::host_time64`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn host_get_time64(host: Option<&&Host>) -> Result<TimeValue64, Error> {
    let time = clock::time::host_time64(host.copied());
    Ok(TimeValue64 {
        seconds: time.seconds,
        nanoseconds: time.nanoseconds,
    })
}

/// Serves `host_get_kernel_version`: the kernel's version for `host`.
fn host_get_kernel_version(host: Option<&&Host>) -> Result<StringC<512>, Error> {
    let mut kernel_version = StringC([0; 512]);
    host::kernel_version(host.copied(), &mut kernel_version.0)?;
    Ok(kernel_version)
}

/// Serves `host_get_uptime64`: the time since boot for `host`.
///
/// # Panics
///
/// As `clock::time::host_uptime64`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn host_get_uptime64(host: Option<&&Host>) -> Result<TimeValue64, Error> {
    let uptime = clock::time::host_uptime64(host.copied());
    Ok(TimeValue64 {
        seconds: uptime.seconds,
        nanoseconds: uptime.nanoseconds,
    })
}

/// Serves `task_max_priority`: caps `task`'s priority at `max_priority`
/// for `host`; its priority set to the cap for `set_priority`, its threads'
/// too for `change_threads`.
///
/// # Errors
///
/// `InvalidArgument` for no task or a priority past the lowest; as
/// `Task::set_max_priority`.
///
/// # Panics
///
/// As `Task::set_max_priority`.
fn task_max_priority(
    host: &Port,
    task: Option<&Task>,
    max_priority: i32,
    set_priority: Boolean,
    change_threads: Boolean,
) -> Result<(), Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    let max_priority = u32::try_from(max_priority)
        .map_err(|_| Error::InvalidArgument)
        .and_then(Priority::try_from)?;
    let base = if bool::from(set_priority) {
        BasePriority::Set
    } else {
        BasePriority::Capped
    };
    let threads = if bool::from(change_threads) {
        Threads::Changed
    } else {
        Threads::Kept
    };
    task.set_max_priority(host, max_priority, base, threads)
}
