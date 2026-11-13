// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The port calls' server routines, as MIG makes them from `mach_port`'s
//! definitions; only those written so far.

use super::mig::{self, OutOfLine};
use crate::error::Error;
use crate::ipc::port::PortName;
use crate::ipc::space::Space;
use crate::{ipc, task};

mig::subsystem! {
    base 3200;
    0 mach_port_names(task = task::space_of) -> (names: OutOfLine<PortName>, types: OutOfLine<u32>);
}

/// Serves `mach_port_names`: the names `space` holds rights under, and
/// their types.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::names`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_names(space: Option<&Space>) -> Result<(OutOfLine<PortName>, OutOfLine<u32>), Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_names: KERN_INVALID_TASK, not yet"));
    let (names, types, count) = ipc::mach_port::names(space);
    Ok((OutOfLine::new(names, count), OutOfLine::new(types, count)))
}
