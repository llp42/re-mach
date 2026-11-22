// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The port calls' server routines, as MIG makes them from `mach_port`'s
//! definitions; only those written so far.

use super::mig;
use super::mig::argument::OutOfLine;
use crate::error::Error;
use crate::ipc::MessageType;
use crate::ipc::port::port::Port;
use crate::ipc::port::port_name::{PortName, PortType};
use crate::ipc::port::send_right::SendRight;
use crate::ipc::space::Space;
use crate::reference::Reference;

mig::subsystem! {
    base 3200;
    0 mach_port_names(task = Port::task_space) -> (names: OutOfLine<PortName>, types: OutOfLine<PortType>);
    1 mach_port_type(task = Port::task_space, name: PortName) -> (port_type: PortType);
    3 mach_port_allocate_name(task = Port::task_space, right: u32, name: PortName);
    4 mach_port_allocate(task = Port::task_space, right: u32) -> (name: PortName);
    5 mach_port_destroy(task = Port::task_space, name: PortName);
    6 mach_port_deallocate(task = Port::task_space, name: PortName);
    7 mach_port_get_refs(task = Port::task_space, name: PortName, right: u32) -> (refs: u32);
    15 mach_port_insert_right(task = Port::task_space, name: PortName, poly: SendRight);
}

/// Serves `mach_port_names`: the names `space` holds rights under, and
/// their types.
///
/// # Panics
///
/// With no space: not yet. As `Space::names`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_names(
    space: Option<&Reference<Space>>,
) -> Result<(OutOfLine<PortName>, OutOfLine<PortType>), Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_names: KERN_INVALID_TASK, not yet"));
    let (names, types, count) = space.names();
    Ok((OutOfLine::new(names, count), OutOfLine::new(types, count)))
}

/// Serves `mach_port_type`: the type of `name`'s rights in `space`.
///
/// # Panics
///
/// With no space: not yet. As `Space::port_type`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_type(space: Option<&Reference<Space>>, name: PortName) -> Result<PortType, Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_type: KERN_INVALID_TASK, not yet"));
    Ok(space.port_type(name))
}

/// Serves `mach_port_allocate_name`: a new right of the kind numbered
/// `right` in `space`, as `name`.
///
/// # Panics
///
/// With no space: not yet. As `Space::allocate_as`.
fn mach_port_allocate_name(
    space: Option<&Reference<Space>>,
    right: u32,
    name: PortName,
) -> Result<(), Error> {
    let space =
        space.unwrap_or_else(|| panic!("mach_port_allocate_name: KERN_INVALID_TASK, not yet"));
    space.allocate_as(right, name)
}

/// Serves `mach_port_allocate`: a new right of the kind numbered `right` in
/// `space`, under a free name, which it gives.
///
/// # Panics
///
/// With no space: not yet. As `Space::allocate`.
fn mach_port_allocate(space: Option<&Reference<Space>>, right: u32) -> Result<PortName, Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_allocate: KERN_INVALID_TASK, not yet"));
    space.allocate(right)
}

/// Serves `mach_port_destroy`: `name`'s rights in `space` are destroyed.
///
/// # Panics
///
/// With no space: not yet. As `Space::destroy_rights`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_destroy(space: Option<&Reference<Space>>, name: PortName) -> Result<(), Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_destroy: KERN_INVALID_TASK, not yet"));
    space.destroy_rights(name);
    Ok(())
}

/// Serves `mach_port_deallocate`: a user reference of `name`'s right in
/// `space` is released.
///
/// # Panics
///
/// With no space: not yet. As `Space::release_user_reference`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_deallocate(space: Option<&Reference<Space>>, name: PortName) -> Result<(), Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_deallocate: KERN_INVALID_TASK, not yet"));
    space.release_user_reference(name);
    Ok(())
}

/// Serves `mach_port_get_refs`: how many references `name` holds in
/// `space` of the kind numbered `right`.
///
/// # Panics
///
/// With no space: not yet. As `Space::user_references`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn mach_port_get_refs(
    space: Option<&Reference<Space>>,
    name: PortName,
    right: u32,
) -> Result<u32, Error> {
    let space = space.unwrap_or_else(|| panic!("mach_port_get_refs: KERN_INVALID_TASK, not yet"));
    Ok(space.user_references(name, right))
}

/// Serves `mach_port_insert_right`: `poly`'s send right goes into `space`
/// as `name`; on a failure it is released, as `ipc_kmsg_destroy` of the
/// request does.
///
/// # Panics
///
/// With no space or no right: not yet. As `Space::insert_right`.
fn mach_port_insert_right(
    space: Option<&Reference<Space>>,
    name: PortName,
    poly: Option<SendRight>,
) -> Result<(), Error> {
    let space =
        space.unwrap_or_else(|| panic!("mach_port_insert_right: KERN_INVALID_TASK, not yet"));
    let poly =
        poly.unwrap_or_else(|| panic!("mach_port_insert_right: KERN_INVALID_CAPABILITY, not yet"));
    space
        .insert_right(name, poly.into_reference(), MessageType::PortSend)
        .map_err(|(error, port)| {
            Port::release_send(port);
            error
        })
}
