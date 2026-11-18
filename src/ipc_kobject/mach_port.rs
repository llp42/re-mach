// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The port calls' server routines, as MIG makes them from `mach_port`'s
//! definitions; only those written so far.

use super::mig::{self, OutOfLine};
use crate::error::Error;
use crate::ipc::port::{PortName, PortType};
use crate::ipc::space::Space;
use crate::reference::Reference;
use crate::{ipc, task};

mig::subsystem! {
    base 3200;
    0 mach_port_names(task = task::space_of) -> (names: OutOfLine<PortName>, types: OutOfLine<PortType>);
    1 mach_port_type(task = task::space_of, name: PortName) -> (port_type: PortType);
    2 mach_port_rename(task = task::space_of, old_name: PortName, new_name: PortName);
    3 mach_port_allocate_name(task = task::space_of, right: u32, name: PortName);
    4 mach_port_allocate(task = task::space_of, right: u32) -> (name: PortName);
    5 mach_port_destroy(task = task::space_of, name: PortName);
    6 mach_port_deallocate(task = task::space_of, name: PortName);
    7 mach_port_get_refs(task = task::space_of, name: PortName, right: u32) -> (refs: u32);
}

/// Serves `mach_port_names`: the names `space` holds rights under, and
/// their types.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::names`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_names(
    space: Option<&Reference<Space>>,
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
fn mach_port_type(space: Option<&Reference<Space>>, name: PortName) -> Result<PortType, Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_type: KERN_INVALID_TASK, not yet"));
    Ok(ipc::mach_port::type_of(space, name))
}

/// Serves `mach_port_rename`: `old_name`'s rights in `space` move to
/// `new_name`.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::rename`.
fn mach_port_rename(
    space: Option<&Reference<Space>>,
    old_name: PortName,
    new_name: PortName,
) -> Result<(), Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_rename: KERN_INVALID_TASK, not yet"));
    ipc::mach_port::rename(space, old_name, new_name)
}

/// Serves `mach_port_allocate_name`: a new right of the kind numbered
/// `right` in `space`, as `name`.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::allocate_name`.
fn mach_port_allocate_name(
    space: Option<&Reference<Space>>,
    right: u32,
    name: PortName,
) -> Result<(), Error> {
    let space =
        space.unwrap_or_else(|| panic!("mach_port_allocate_name: KERN_INVALID_TASK, not yet"));
    ipc::mach_port::allocate_name(space, right, name)
}

/// Serves `mach_port_allocate`: a new right of the kind numbered `right` in
/// `space`, under a free name, which it gives.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::allocate`.
fn mach_port_allocate(space: Option<&Reference<Space>>, right: u32) -> Result<PortName, Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_allocate: KERN_INVALID_TASK, not yet"));
    ipc::mach_port::allocate(space, right)
}

/// Serves `mach_port_destroy`: `name`'s rights in `space` are destroyed.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::destroy`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_destroy(space: Option<&Reference<Space>>, name: PortName) -> Result<(), Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_destroy: KERN_INVALID_TASK, not yet"));
    ipc::mach_port::destroy(space, name);
    Ok(())
}

/// Serves `mach_port_deallocate`: a user reference of `name`'s right in
/// `space` is released.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::deallocate`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_deallocate(space: Option<&Reference<Space>>, name: PortName) -> Result<(), Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_deallocate: KERN_INVALID_TASK, not yet"));
    ipc::mach_port::deallocate(space, name);
    Ok(())
}

/// Serves `mach_port_get_refs`: how many references `name` holds in
/// `space` of the kind numbered `right`.
///
/// # Panics
///
/// With no space: not yet. As `ipc::mach_port::get_refs`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_get_refs(
    space: Option<&Reference<Space>>,
    name: PortName,
    right: u32,
) -> Result<u32, Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_get_refs: KERN_INVALID_TASK, not yet"));
    Ok(ipc::mach_port::get_refs(space, name, right))
}
