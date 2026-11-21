// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The host calls' server routines, as MIG makes them from `mach_host`'s
//! definitions; only those written so far.

use core::mem::ManuallyDrop;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use super::mig::{self, Boolean, StringC};
use crate::error::Error;
use crate::host::{self, HOST, Host};
use crate::ipc::MessageType;
use crate::ipc::port::Port;
use crate::processor::{DEFAULT_SET, ProcessorSet};
use crate::reference::Reference;
use crate::sched::{BasePriority, Priority};
use crate::task::{self, Task, Threads};
use crate::{clock, ipc_host, machine};

mig::subsystem! {
    base 2600;
    6 processor_set_default(host = ipc_host::host) -> (default_set: Port);
    8 processor_set_create(host = ipc_host::privileged_host) -> (new_set: Port, new_name: Port);
    9 processor_set_destroy(set = ipc_host::set_of);
    22 task_priority(task = task::of, priority: i32, change_threads: Boolean);
    34 host_get_time(host = ipc_host::host) -> (current_time: RpcTimeValue);
    35 host_reboot(host = ipc_host::privileged_host, options: i32);
    38 host_info(host = ipc_host::host, flavor: i32) -> (info: [i32; ..1024], CountInOut);
    40 processor_set_info(set_name = ipc_host::set_name_of, flavor: i32)
        -> (host: Port, info_out: [i32; ..1024], CountInOut);
    43 host_get_time64(host = ipc_host::host) -> (current_time: TimeValue64);
    46 host_get_kernel_version(host = ipc_host::host) -> (kernel_version: StringC<512>);
    47 host_get_uptime64(host = ipc_host::host) -> (uptime: TimeValue64);
    49 task_max_priority(
        host: Port,
        task = task::of,
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
unsafe impl mig::Inline for RpcTimeValue {
    const WORD: u64 = mig::type_word(MessageType::Integer64, 64, 2);
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
unsafe impl mig::Inline for TimeValue64 {
    const WORD: u64 = mig::type_word(MessageType::Integer64, 64, 2);
}

/// Serves `processor_set_default`: a send right for the default set's name
/// port, as `convert_pset_name_to_port` gives it.
///
/// # Errors
///
/// `InvalidArgument` for no host.
///
/// # Panics
///
/// As `ProcessorSet::name_port`.
fn processor_set_default(host: Option<&&Host>) -> Result<Reference<Port>, Error> {
    if host.is_none() {
        return Err(Error::InvalidArgument);
    }
    // SAFETY: the default set's own reference never drops; this handle
    // borrows it and never drops it.
    let set = ManuallyDrop::new(unsafe { Reference::from_raw(NonNull::from(&DEFAULT_SET)) });
    Ok(ProcessorSet::name_port(Reference::clone(&set)))
}

/// Serves `processor_set_create`: a new set; send rights for its port and
/// its name port, as `convert_pset_to_port` and `convert_pset_name_to_port`
/// make them.
///
/// # Errors
///
/// `InvalidArgument` for no privileged host; as `ProcessorSet::create`.
///
/// # Panics
///
/// As `ProcessorSet::port` and `ProcessorSet::name_port`.
fn processor_set_create(host: Option<&&Host>) -> Result<(Reference<Port>, Reference<Port>), Error> {
    if host.is_none() {
        return Err(Error::InvalidArgument);
    }
    let set = NonNull::from(ProcessorSet::create()?);
    // SAFETY: the references `create` took for the caller's set and name.
    let (new_set, new_name) = unsafe { (Reference::from_raw(set), Reference::from_raw(set)) };
    Ok((
        ProcessorSet::port(new_set),
        ProcessorSet::name_port(new_name),
    ))
}

/// Serves `processor_set_destroy`: ends `set`.
///
/// # Errors
///
/// `InvalidArgument` for no set; as `ProcessorSet::destroy`.
///
/// # Panics
///
/// As `ProcessorSet::destroy`.
fn processor_set_destroy(set: Option<&Reference<ProcessorSet>>) -> Result<(), Error> {
    let Some(set) = set else {
        return Err(Error::InvalidArgument);
    };
    set.destroy()
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

/// Serves `processor_set_info`: up to `info_out.len()` integers of
/// `flavor` for `set_name`; gives a send right for the host's port, as
/// `convert_host_to_port` makes it, and how many.
///
/// # Errors
///
/// `InvalidArgument` for no set or a flavor Mach lacks; as
/// `ProcessorSet::sched_info`.
///
/// # Panics
///
/// If the host has no port. For `PROCESSOR_SET_BASIC_INFO`: not yet.
fn processor_set_info(
    set_name: Option<&Reference<ProcessorSet>>,
    flavor: i32,
    info_out: &mut [i32],
) -> Result<(Reference<Port>, usize), Error> {
    let Some(set) = set_name else {
        return Err(Error::InvalidArgument);
    };
    // PROCESSOR_SET_BASIC_INFO, PROCESSOR_SET_SCHED_INFO.
    let count = match flavor {
        1 => panic!("processor_set_info: PROCESSOR_SET_BASIC_INFO, not yet"),
        2 => set.sched_info(info_out)?,
        _ => return Err(Error::InvalidArgument),
    };
    let port = NonNull::new(HOST.port.load(Ordering::Acquire))
        .expect("convert_host_to_port: no host port");
    // SAFETY: the host's slot keeps its port's reference for good, which
    // the handle borrows and never drops.
    let port = ManuallyDrop::new(unsafe { Reference::from_raw(port) });
    Ok((Port::make_send(&port), count))
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
