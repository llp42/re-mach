// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! GNU Mach's own calls' server routines, as MIG makes them from
//! `gnumach`'s definitions; only those written so far.

use super::mig;
use crate::error::Error;
use crate::ipc::port::Port;
use crate::reference::Reference;
use crate::vm::map::Map;
use crate::{task, vm};

mig::subsystem! {
    base 4200;
    7 vm_wire_all(host: Port, task = task::map_of, flags: u32);
    15 vm_set_size_limit(host_port: Port, map = task::map_of, current_limit: usize, max_limit: usize);
    16 vm_get_size_limit(map = task::map_of) -> (current_limit: usize, max_limit: usize);
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
