// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The port calls' server routines, as MIG makes them from `mach_port`'s
//! definitions; only those written so far.

use super::mig::{self, OutOfLine};
use crate::error::Error;
use crate::ipc::port::{PortName, PortType};
use crate::ipc::space::Space;
use crate::{ipc, task};

mig::subsystem! {
    base 3200;
    0 mach_port_names(task = task::space_of) -> (names: OutOfLine<PortName>, types: OutOfLine<PortType>);
    1 mach_port_type(task = task::space_of, name: PortName) -> (port_type: PortType);
}

/// Serves `mach_port_names`: the names `space` holds rights under, and
/// their types.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::names`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_names(
    space: Option<&Space>,
) -> Result<(OutOfLine<PortName>, OutOfLine<PortType>), Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_names: KERN_INVALID_TASK, not yet"));
    let (names, types, count) = ipc::mach_port::names(space);
    Ok((OutOfLine::new(names, count), OutOfLine::new(types, count)))
}

/// Serves `mach_port_type`: the type of `name`'s rights in `space`.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::type_of`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_type(space: Option<&Space>, name: PortName) -> Result<PortType, Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_type: KERN_INVALID_TASK, not yet"));
    Ok(ipc::mach_port::type_of(space, name))
}
