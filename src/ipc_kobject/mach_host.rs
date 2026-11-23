// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The host calls' server routines, as MIG makes them from `mach_host`'s
//! definitions; only those written so far.

use core::ptr::{self, NonNull};

use super::mig;
use super::mig::boolean::Boolean;
use super::mig::port_array::PortArray;
use super::mig::string_char::StringC;
use crate::error::Error;
use crate::host::{self, HOST, Host};
use crate::ipc::MessageType;
use crate::ipc::port::port::Port;
use crate::processor::all_sets::BOOT_PROCESSOR;
use crate::processor::processor::Processor;
use crate::processor::set::ProcessorSet;
use crate::reference::Reference;
use crate::sched::priority::{BasePriority, Priority};
use crate::smp;
use crate::task::ports::Threads;
use crate::task::task::Task;
use crate::thread::thread::Thread;
use crate::vm::{self, VirtualAddress};
use crate::{clock, machine};

mig::subsystem! {
    base 2600;
    15 thread_get_assignment(thread = Port::to_thread) -> (assigned_set: Port);
    16 task_assign(task = Port::to_task, new_set = Port::to_processor_set, assign_threads: Boolean);
    20 thread_priority(thread = Port::to_thread, priority: i32, set_max: Boolean);
    21 thread_max_priority(
        thread = Port::to_thread,
        processor_set = Port::to_processor_set,
        max_priority: i32
    );
    22 task_priority(task = Port::to_task, priority: i32, change_threads: Boolean);
    27 processor_set_tasks(processor_set = Port::to_processor_set) -> (task_list: PortArray);
    29 host_processor_sets(host = Port::as_host) -> (processor_sets: PortArray);
    30 host_processor_set_priv(
        host = Port::as_privileged_host,
        set_name = Port::to_processor_set_name
    ) -> (set: Port);
    34 host_get_time(host = Port::as_host) -> (current_time: RpcTimeValue);
    35 host_reboot(host = Port::as_privileged_host, options: i32);
    36 vm_wire(host: Port, task = Port::task_map, address: usize, size: usize, access: u32);
    37 thread_wire(host = Port::as_privileged_host, thread = Port::to_thread, wired: Boolean);
    38 host_info(host = Port::as_host, flavor: i32) -> (info: [i32; ..1024], CountInOut);
    39 processor_info(processor = Port::to_processor_name, flavor: i32)
        -> (host: Port, info: [i32; ..1024], CountInOut);
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

/// Serves `thread_get_assignment`: a send right for `thread`'s set's name
/// port.
///
/// # Errors
///
/// `InvalidArgument` for no thread or one in no set.
///
/// # Panics
///
/// If the set is being freed: not yet.
fn thread_get_assignment(thread: Option<&Reference<Thread>>) -> Result<Reference<Port>, Error> {
    let Some(thread) = thread else {
        return Err(Error::InvalidArgument);
    };
    let Some(set) = thread.processor_set() else {
        return Err(Error::InvalidArgument);
    };
    // SAFETY: the thread's set is in use while it has the thread.
    let set = unsafe { Reference::upgrade(NonNull::from(set)) }
        .expect("thread_get_assignment: a set being freed");
    Ok(ProcessorSet::name_port(set))
}

/// Serves `task_assign`: `task` moved to `new_set`, its threads as
/// `assign_threads` says.
///
/// # Errors
///
/// `InvalidArgument` for no task or set; as `Task::assign`.
fn task_assign(
    task: Option<&Reference<Task>>,
    new_set: Option<&ProcessorSet>,
    assign_threads: Boolean,
) -> Result<(), Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    let Some(set) = new_set else {
        return Err(Error::InvalidArgument);
    };
    // SAFETY: the set's port keeps it alive while its reference is held.
    let set =
        unsafe { Reference::upgrade(NonNull::from(set)) }.expect("task_assign: a set being freed");
    let threads = if bool::from(assign_threads) {
        Threads::Changed
    } else {
        Threads::Kept
    };
    task.assign(set, threads)
}

/// Serves `thread_priority`: `thread`'s base priority `priority`, and its
/// cap too if `set_max` says.
///
/// # Errors
///
/// `InvalidArgument` for no thread or a priority past the lowest; as
/// `Thread::set_priority_and_max`.
fn thread_priority(
    thread: Option<&Reference<Thread>>,
    priority: i32,
    set_max: Boolean,
) -> Result<(), Error> {
    let Some(thread) = thread else {
        return Err(Error::InvalidArgument);
    };
    let priority = u32::try_from(priority).map_err(|_| Error::InvalidArgument)?;
    let priority = Priority::try_from(priority)?;
    thread.set_priority_and_max(priority, bool::from(set_max))
}

/// Serves `thread_max_priority`: `thread`'s cap `max_priority`, in `set`.
///
/// # Errors
///
/// `InvalidArgument` for no thread or set, or a priority past the lowest,
/// and `Failure` if the thread is in another set.
fn thread_max_priority(
    thread: Option<&Reference<Thread>>,
    set: Option<&ProcessorSet>,
    max_priority: i32,
) -> Result<(), Error> {
    let Some(thread) = thread else {
        return Err(Error::InvalidArgument);
    };
    let Some(set) = set else {
        return Err(Error::InvalidArgument);
    };
    let max_priority = u32::try_from(max_priority).map_err(|_| Error::InvalidArgument)?;
    let max_priority = Priority::try_from(max_priority)?;
    if !thread.is_in(set) {
        return Err(Error::Failure);
    }
    thread.set_max_priority(max_priority);
    Ok(())
}

/// Serves `processor_set_tasks`: send rights for the ports of `set`'s
/// tasks, in an out-of-line array.
///
/// # Errors
///
/// `InvalidArgument` for no set; as `ProcessorSet::task_ports`.
fn processor_set_tasks(set: Option<&Reference<ProcessorSet>>) -> Result<PortArray, Error> {
    let Some(set) = set else {
        return Err(Error::InvalidArgument);
    };
    let ports = set.task_ports()?;
    // SAFETY: a kalloc array of its size holding ports' exposed addresses,
    // each with its reference, as `ProcessorSet::task_ports` gives it.
    Ok(unsafe { PortArray::new(ports) })
}

/// Serves `host_processor_sets`: send rights for every set's name port, in
/// an out-of-line array.
///
/// # Errors
///
/// `InvalidHost` for no host; as `ProcessorSet::name_ports`.
fn host_processor_sets(host: Option<&&Host>) -> Result<PortArray, Error> {
    if host.is_none() {
        return Err(Error::InvalidHost);
    }
    let ports = ProcessorSet::name_ports()?;
    // SAFETY: a kalloc array of its size holding ports' exposed addresses,
    // each with its reference, as `ProcessorSet::name_ports` gives it.
    Ok(unsafe { PortArray::new(ports) })
}

/// Serves `host_processor_set_priv`: a send right for `set_name`'s set's
/// control port.
///
/// # Errors
///
/// `InvalidHost` for no privileged host, and `InvalidArgument` for no set
/// name.
///
/// # Panics
///
/// If the set is being freed: not yet.
fn host_processor_set_priv(
    host: Option<&&Host>,
    set_name: Option<&ProcessorSet>,
) -> Result<Reference<Port>, Error> {
    if host.is_none() {
        return Err(Error::InvalidHost);
    }
    let Some(set) = set_name else {
        return Err(Error::InvalidArgument);
    };
    // SAFETY: the set's name port keeps it alive while its right is held.
    let set = unsafe { Reference::upgrade(NonNull::from(set)) }
        .expect("host_processor_set_priv: a set being freed");
    Ok(ProcessorSet::port(set))
}

/// Serves `vm_wire`: `size` bytes of `map` from `address` wired for
/// `access`, or unwired for none, for `host`.
///
/// # Errors
///
/// As `vm::user::wire`.
fn vm_wire(
    host: &Port,
    map: Option<&crate::vm::map::map::Map>,
    address: usize,
    size: usize,
    access: u32,
) -> Result<(), Error> {
    vm::user::wire(host, map, VirtualAddress::from(address), size, access)
}

/// Serves `thread_wire`: `thread` kept able to run and to allocate memory,
/// or no longer, for `host`. Its kernel stack is wired always, and nothing
/// here allocates memory for the privilege.
///
/// # Errors
///
/// `InvalidArgument` unless `host` is the privileged host and `thread` is
/// the current one.
fn thread_wire(
    host: Option<&&Host>,
    thread: Option<&Thread>,
    _wired: Boolean,
) -> Result<(), Error> {
    if host.is_none() {
        return Err(Error::InvalidArgument);
    }
    let Some(thread) = thread else {
        return Err(Error::InvalidArgument);
    };
    let Some(current) = smp::current().active_thread() else {
        return Err(Error::InvalidArgument);
    };
    if !ptr::eq(thread, current) {
        return Err(Error::InvalidArgument);
    }
    Ok(())
}

/// Serves `processor_info`: `processor`'s information of `flavor`, into
/// `info` cut to the count asked for, and a send right for its host.
///
/// # Errors
///
/// `InvalidArgument` for no processor or a count short of the record, and
/// `Failure` for another flavor.
///
/// # Panics
///
/// If the processor has no machine slot: not yet.
fn processor_info(
    processor: Option<&&Processor>,
    flavor: i32,
    info: &mut [i32],
) -> Result<(Reference<Port>, usize), Error> {
    let Some(processor) = processor else {
        return Err(Error::InvalidArgument);
    };
    // PROCESSOR_BASIC_INFO and PROCESSOR_BASIC_INFO_COUNT.
    if flavor != 1 {
        return Err(Error::Failure);
    }
    if info.len() < 5 {
        return Err(Error::InvalidArgument);
    }
    let slot = processor.slot();
    let slot_info = machine::slot(slot).expect("processor_info: no machine slot");
    info[0] = slot_info.ty.0;
    info[1] = slot_info.subtype.0;
    info[2] = i32::from(processor.is_running());
    info[3] = i32::try_from(slot).unwrap();
    info[4] = i32::from(
        BOOT_PROCESSOR
            .get()
            .is_some_and(|boot| ptr::eq(*boot, *processor)),
    );
    Ok((HOST.port(), 5))
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
