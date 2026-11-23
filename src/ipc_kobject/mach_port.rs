// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The port calls' server routines, as MIG makes them from `mach_port`'s
//! definitions; only those written so far.

use super::mig;
use super::mig::argument::{Inline, OutOfLine, type_word};
use crate::error::Error;
use crate::host::Host;
use crate::ipc::MessageType;
use crate::ipc::mach_port::Status;
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
    8 mach_port_mod_refs(task = Port::task_space, name: PortName, right: u32, delta: i32);
    10 mach_port_set_qlimit(task = Port::task_space, name: PortName, qlimit: u32);
    12 mach_port_get_set_status(task = Port::task_space, name: PortName) -> (members: OutOfLine<PortName>);
    13 mach_port_move_member(task = Port::task_space, member: PortName, after: PortName);
    15 mach_port_insert_right(task = Port::task_space, name: PortName, poly: SendRight);
    16 mach_port_extract_right(task = Port::task_space, name: PortName, msgt_name: u32) -> (poly: Port);
    17 mach_port_get_receive_status(task = Port::task_space, name: PortName) -> (status: Status);
    21 mach_port_set_protected_payload(task = Port::task_space, name: PortName, payload: usize);
    22 mach_port_clear_protected_payload(task = Port::task_space, name: PortName);
    23 mach_port_set_ktype(
        host = Port::as_privileged_host,
        task = Port::task_space,
        name: PortName,
        right: u32,
        ktype: u32
    );
}

// SAFETY: `Status` is `repr(C)`, nine `u32`s.
unsafe impl Inline for Status {
    const WORD: u64 = type_word(MessageType::Integer32, 32, 9);
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

/// Serves `mach_port_mod_refs`: adds `delta` to `name`'s user references of
/// the right numbered `right` in `space`.
///
/// # Errors
///
/// As `Space::mod_refs`.
fn mach_port_mod_refs(
    space: Option<&Reference<Space>>,
    name: PortName,
    right: u32,
    delta: i32,
) -> Result<(), Error> {
    let Some(space) = space else {
        return Err(Error::InvalidTask);
    };
    space.mod_refs(name, right, delta)
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

/// Serves `mach_port_set_qlimit`: `name`'s receive right in `space` queues
/// at most `qlimit` messages.
///
/// # Errors
///
/// As `Space::set_queue_limit`.
fn mach_port_set_qlimit(
    space: Option<&Reference<Space>>,
    name: PortName,
    qlimit: u32,
) -> Result<(), Error> {
    let Some(space) = space else {
        return Err(Error::InvalidTask);
    };
    space.set_queue_limit(name, qlimit)
}

/// Serves `mach_port_get_set_status`: the names of `name`'s port set's
/// members in `space`, in an out-of-line array.
///
/// # Errors
///
/// As `Space::set_members`.
fn mach_port_get_set_status(
    space: Option<&Reference<Space>>,
    name: PortName,
) -> Result<OutOfLine<PortName>, Error> {
    let Some(space) = space else {
        return Err(Error::InvalidTask);
    };
    let (members, count) = space.set_members(name)?;
    Ok(members.map_or_else(OutOfLine::empty, |copy| OutOfLine::new(copy, count)))
}

/// Serves `mach_port_move_member`: `member`'s port in `space` into the set
/// `after` names, or out of its set for `MACH_PORT_NULL`.
///
/// # Errors
///
/// As `Space::move_member`.
fn mach_port_move_member(
    space: Option<&Reference<Space>>,
    member: PortName,
    after: PortName,
) -> Result<(), Error> {
    let Some(space) = space else {
        return Err(Error::InvalidTask);
    };
    space.move_member(member, after)
}

/// Serves `mach_port_extract_right`: the right `msgt_name` names for
/// `name` in `space`, extracted as if `space` sent it.
///
/// # Errors
///
/// `InvalidTask` for no space, `InvalidValue` for no port right; as
/// `Space::extract_right`.
///
/// # Panics
///
/// For a right whose copy-in is not written: not yet.
fn mach_port_extract_right(
    space: Option<&Reference<Space>>,
    name: PortName,
    msgt_name: u32,
) -> Result<Reference<Port>, Error> {
    let Some(space) = space else {
        return Err(Error::InvalidTask);
    };
    // MACH_MSG_TYPE_PORT_ANY: from MOVE_RECEIVE to MAKE_SEND_ONCE.
    if !(16..=21).contains(&msgt_name) {
        return Err(Error::InvalidValue);
    }
    let Ok(msgt_name) = u8::try_from(msgt_name) else {
        return Err(Error::InvalidValue);
    };
    let Ok(message_type) = MessageType::try_from(msgt_name) else {
        panic!("mach_port_extract_right: type {msgt_name}, not yet");
    };
    if !matches!(
        message_type,
        MessageType::CopySend | MessageType::MakeSend | MessageType::PortSend
    ) {
        panic!("mach_port_extract_right: type {msgt_name}, not yet");
    }
    Ok(space.extract_right(name, message_type))
}

/// Serves `mach_port_get_receive_status`: the status of `name`'s receive
/// right in `space`.
///
/// # Errors
///
/// As `Space::receive_status`.
fn mach_port_get_receive_status(
    space: Option<&Reference<Space>>,
    name: PortName,
) -> Result<Status, Error> {
    let Some(space) = space else {
        return Err(Error::InvalidTask);
    };
    space.receive_status(name)
}

/// Serves `mach_port_set_protected_payload`: `name`'s receive right in
/// `space` carries `payload` in place of its name.
///
/// # Errors
///
/// As `Space::set_protected_payload`.
fn mach_port_set_protected_payload(
    space: Option<&Reference<Space>>,
    name: PortName,
    payload: usize,
) -> Result<(), Error> {
    let Some(space) = space else {
        return Err(Error::InvalidTask);
    };
    space.set_protected_payload(name, Some(u64::try_from(payload).unwrap()))
}

/// Serves `mach_port_clear_protected_payload`: `name`'s receive right in
/// `space` carries its name again.
///
/// # Errors
///
/// As `Space::set_protected_payload`.
fn mach_port_clear_protected_payload(
    space: Option<&Reference<Space>>,
    name: PortName,
) -> Result<(), Error> {
    let Some(space) = space else {
        return Err(Error::InvalidTask);
    };
    space.set_protected_payload(name, None)
}

/// Serves `mach_port_set_ktype`: `name`'s port in `space` stands for a
/// user device, or nothing, for `host`.
///
/// # Errors
///
/// `InvalidHost` unless `host` is the privileged host; as `Space::set_ktype`.
fn mach_port_set_ktype(
    host: Option<&&Host>,
    task: Option<&Space>,
    name: PortName,
    right: u32,
    ktype: u32,
) -> Result<(), Error> {
    if host.is_none() {
        return Err(Error::InvalidHost);
    }
    let Some(task) = task else {
        return Err(Error::InvalidTask);
    };
    task.set_ktype(name, right, ktype)
}
