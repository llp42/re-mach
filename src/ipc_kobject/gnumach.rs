// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! GNU Mach's own calls' server routines, as MIG makes them from
//! `gnumach`'s definitions; only those written so far.

use core::sync::atomic::Ordering;

use super::mig;
use super::mig::boolean::Boolean;
use super::mig::string_char::StringChar;
use crate::error::Error;
use crate::gsync::{self, Flags};
use crate::host::Host;
use crate::ipc::port::port::Port;
use crate::ipc::port::send_right::SendRight;
use crate::reference::Reference;
use crate::task::task::Task;
use crate::vm::map::map::Map;
use crate::vm::{self, VirtualAddress};

mig::subsystem! {
    base 4200;
    0 vm_cache_statistics(target_task = Port::to_task) -> (vm_cache_stats: [i32; 11]);
    2 task_set_name(task = Port::to_task, name: [StringChar; ..64]);
    3 register_new_task_notification(host = Port::as_privileged_host, notification: SendRight);
    4 gsync_wait(
        task = Port::to_task,
        address: VirtualAddress,
        value: u32,
        value2: u32,
        milliseconds: u32,
        flags: i32
    );
    5 gsync_wake(task = Port::to_task, address: VirtualAddress, value: u32, flags: i32);
    7 vm_wire_all(host: Port, task = Port::task_map, flags: u32);
    10 vm_allocate_contiguous(
        host = Port::as_privileged_host,
        target_task = Port::task_map,
        size: usize,
        pmin: usize,
        pmax: usize,
        palign: usize
    ) -> (vaddr: usize, paddr: usize);
    11 task_set_essential(task = Port::to_task, essential: Boolean);
    15 vm_set_size_limit(host_port: Port, map = Port::task_map, current_limit: usize, max_limit: usize);
    16 vm_get_size_limit(map = Port::task_map) -> (current_limit: usize, max_limit: usize);
}

/// Serves `vm_cache_statistics`: the page cache statistics of the host
/// `task` is on.
///
/// # Errors
///
/// `InvalidArgument` for no task.
fn vm_cache_statistics(task: Option<&Reference<Task>>) -> Result<[i32; 11], Error> {
    if task.is_none() {
        return Err(Error::InvalidArgument);
    }
    Ok(vm::user::cache_statistics())
}

/// Serves `task_set_name`: names `task` `name`.
///
/// # Errors
///
/// `InvalidArgument` for no task.
fn task_set_name(task: Option<&Reference<Task>>, name: &[StringChar]) -> Result<(), Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    task.set_name(name.iter().map(|&character| u8::from(character)));
    Ok(())
}

/// Serves `register_new_task_notification`: `notification` gets a message
/// for every new task, once per boot.
///
/// # Errors
///
/// `InvalidHost` for no privileged host, `NoAccess` if one is registered.
fn register_new_task_notification(
    host: Option<&&Host>,
    notification: Option<SendRight>,
) -> Result<(), Error> {
    let Some(host) = host else {
        return Err(Error::InvalidHost);
    };
    host.register_new_task_notification(notification)
}

/// Serves `task_set_essential`: `task` is essential, or no longer, as
/// `essential` says.
///
/// # Errors
///
/// `InvalidArgument` for no task.
fn task_set_essential(task: Option<&Reference<Task>>, essential: Boolean) -> Result<(), Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    task.essential
        .store(bool::from(essential), Ordering::Relaxed);
    Ok(())
}

/// Serves `gsync_wait`: the current thread waits on the word at `address`
/// in `task` while it holds `value` (and the next `value2`), as `flags`
/// say, for at most `milliseconds` if timed.
///
/// # Errors
///
/// As `gsync::wait`.
///
/// # Panics
///
/// As `gsync::wait`.
fn gsync_wait(
    task: Option<&Reference<Task>>,
    address: VirtualAddress,
    value: u32,
    value2: u32,
    milliseconds: u32,
    flags: i32,
) -> Result<(), Error> {
    gsync::wait(
        task.map(|task| &**task),
        address,
        value,
        value2,
        milliseconds,
        Flags::from_bits_retain(flags),
    )
}

/// Serves `gsync_wake`: stores `value` in the word at `address` in `task`
/// and wakes that word's waiters, as `flags` say.
///
/// # Errors
///
/// As `gsync::wake`.
///
/// # Panics
///
/// As `gsync::wake`.
fn gsync_wake(
    task: Option<&Reference<Task>>,
    address: VirtualAddress,
    value: u32,
    flags: i32,
) -> Result<(), Error> {
    gsync::wake(
        task.map(|task| &**task),
        address,
        value,
        Flags::from_bits_retain(flags),
    )
}

/// Serves `vm_wire_all`: all of `map` wired as `flags` say, or unwired,
/// for `host`.
///
/// # Panics
///
/// As `vm::user::wire_all`.
fn vm_wire_all(host: &Port, map: Option<&Map>, flags: u32) -> Result<(), Error> {
    vm::user::wire_all(host, map, flags)
}

/// Serves `vm_allocate_contiguous`: `size` bytes of contiguous physical
/// memory from `pmin` up to `pmax`, aligned to `palign`, mapped into
/// `map` for reading and writing, for `host`.
///
/// # Errors
///
/// `InvalidHost` unless `host` is the privileged host, and `InvalidTask`
/// for no map; as `vm::user::allocate_contiguous`.
fn vm_allocate_contiguous(
    host: Option<&&Host>,
    map: Option<&Map>,
    size: usize,
    pmin: usize,
    pmax: usize,
    palign: usize,
) -> Result<(usize, usize), Error> {
    if host.is_none() {
        return Err(Error::InvalidHost);
    }
    let Some(map) = map else {
        return Err(Error::InvalidTask);
    };
    let (address, physical) = vm::user::allocate_contiguous(map, size, pmin, pmax, palign)?;
    Ok((address.into(), physical))
}

/// Serves `vm_set_size_limit`: `map` limited to `current` bytes, at most
/// `max` once raised, for `host`.
///
/// # Errors
///
/// As `vm::user::set_size_limit`.
fn vm_set_size_limit(
    host: &Port,
    map: Option<&Map>,
    current: usize,
    max: usize,
) -> Result<(), Error> {
    vm::user::set_size_limit(host, map, current, max)
}

/// Serves `vm_get_size_limit`: `map`'s size limit, and the most it may be
/// raised to.
///
/// # Errors
///
/// `InvalidTask` for no map.
fn vm_get_size_limit(map: Option<&Reference<Map>>) -> Result<(usize, usize), Error> {
    let Some(map) = map else {
        return Err(Error::InvalidTask);
    };
    Ok(vm::user::size_limits(map))
}
