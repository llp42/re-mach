// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! GNU Mach's own calls' server routines, as MIG makes them from
//! `gnumach`'s definitions; only those written so far.

use super::mig;
use crate::error::Error;
use crate::ipc::port::Port;
use crate::vm::map::Map;
use crate::{task, vm};

mig::subsystem! {
    base 4200;
    7 vm_wire_all(host: Port, task = task::map_of, flags: u32);
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
