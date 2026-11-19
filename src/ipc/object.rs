// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Objects: what a space's names stand for, ports and port sets.

use core::ptr::NonNull;

use super::entry::Rights;
use super::port::{Port, PortName};
use super::port_set::PortSet;
use super::space::Space;
use super::{MessageType, right};
use crate::error::Error;
use crate::reference::Reference;
use crate::slab::CacheBox;

/// What an entry's rights are for.
pub(super) enum Object {
    /// A port, by its reference.
    Port(Reference<Port>),
    /// A port set.
    PortSet(NonNull<PortSet>),
}

/// Puts the right for `object`, new, in `space` under a free name, as
/// `ipc_object_alloc` does, and gives the name: the receive right for a
/// port, the port set right for a set, `kind` telling which. `set_name`
/// gives the object its name; the entry takes the right and the object's
/// reference, linked only once the object knows the name.
///
/// # Errors
///
/// `ResourceShortage` if no entry or map node can be allocated; the object
/// is freed.
///
/// # Panics
///
/// As `SpaceState::allocate`.
pub(super) fn allocate<T>(
    space: &Space,
    mut object: CacheBox<T>,
    set_name: fn(&mut T, PortName),
    kind: fn(CacheBox<T>) -> Object,
) -> Result<PortName, Error> {
    let mut state = space.state.write();
    let (name, entry) = state.allocate()?;
    set_name(&mut object, name);
    let object = kind(object);
    // The right, with no user references.
    entry.bits.rights |= match object {
        Object::Port(_) => Rights::RECEIVE,
        Object::PortSet(_) => Rights::PORT_SET,
    };
    entry.object = Some(object);
    drop(state);
    Ok(name)
}

/// Puts the right for `object`, new, in `space` as `name`, as
/// `ipc_object_alloc_name` does: the receive right for a port, the port set
/// right for a set, `kind` telling which. The entry takes the right and the
/// object's reference.
///
/// # Errors
///
/// `ResourceShortage` if no entry or map node can be allocated; the object
/// is freed.
///
/// # Panics
///
/// If `name` holds rights: not yet. As `SpaceState::allocate_name`.
pub(super) fn allocate_name<T>(
    space: &Space,
    object: CacheBox<T>,
    name: PortName,
    kind: fn(CacheBox<T>) -> Object,
) -> Result<(), Error> {
    let mut state = space.state.write();
    let entry = state.allocate_name(name)?;
    // As ipc_right_inuse.
    assert!(
        !entry.in_use(),
        "ipc_object_alloc_name: KERN_NAME_EXISTS, not yet"
    );
    let object = kind(object);
    // The right, with no user references.
    entry.bits.rights |= match object {
        Object::Port(_) => Rights::RECEIVE,
        Object::PortSet(_) => Rights::PORT_SET,
    };
    entry.object = Some(object);
    drop(state);
    Ok(())
}

/// Puts a dead name, new, in `space` under a free name, as
/// `ipc_object_alloc_dead` does, and gives the name.
///
/// # Errors
///
/// `ResourceShortage` if no entry or map node can be allocated.
///
/// # Panics
///
/// If the entry has an object. As `SpaceState::allocate`.
pub(super) fn allocate_dead(space: &Space) -> Result<PortName, Error> {
    let mut state = space.state.write();
    let (name, entry) = state.allocate()?;
    assert!(
        entry.object.is_none(),
        "ipc_object_alloc_dead: an entry with an object"
    );
    // No object, and one user reference.
    entry.bits.rights |= Rights::DEAD_NAME;
    entry.bits.user_references = 1;
    drop(state);
    Ok(name)
}

/// Puts a dead name, new, in `space` as `name`, as
/// `ipc_object_alloc_dead_name` does.
///
/// # Errors
///
/// `ResourceShortage` if no entry or map node can be allocated.
///
/// # Panics
///
/// If the entry has an object. If `name` holds rights: not yet. As
/// `SpaceState::allocate_name`.
pub(super) fn allocate_dead_name(space: &Space, name: PortName) -> Result<(), Error> {
    let mut state = space.state.write();
    let entry = state.allocate_name(name)?;
    // As ipc_right_inuse.
    assert!(
        !entry.in_use(),
        "ipc_object_alloc_dead_name: KERN_NAME_EXISTS, not yet"
    );
    assert!(
        entry.object.is_none(),
        "ipc_object_alloc_dead_name: an entry with an object"
    );
    // No object, and one user reference.
    entry.bits.rights |= Rights::DEAD_NAME;
    entry.bits.user_references = 1;
    drop(state);
    Ok(())
}

/// Copies in the right `message_type` names from `name` in `space`, as
/// `ipc_object_copyin` does: gives the port's new right with its reference.
///
/// # Panics
///
/// For a type that is not a right. For a right other than a copied or made
/// send right: not yet. As `right::lookup_write`, `right::copy_in` and
/// `right::make_send`.
pub(super) fn copy_in(space: &Space, name: PortName, message_type: MessageType) -> Reference<Port> {
    let state = right::lookup_write(space, name);
    let port = match message_type {
        MessageType::CopySend => right::copy_in(&state, name),
        MessageType::MakeSend => right::make_send(space, &state, name),
        MessageType::PortSend | MessageType::PortSendOnce | MessageType::MakeSendOnce => {
            panic!("ipc_right_copyin: MOVE_SEND, MOVE_SEND_ONCE and MAKE_SEND_ONCE, not yet")
        }
        _ => panic!("ipc_right_copyin: strange rights"),
    };
    drop(state);
    port
}

/// Puts the right `message_type` names, for `port`, in `space` under a
/// free name, as `ipc_object_copyout` does, and gives the name; the space
/// takes the right and its reference.
///
/// # Errors
///
/// `ResourceShortage` if no entry or map node can be allocated; it gives
/// the right back.
///
/// # Panics
///
/// If `space` or `port` is dead: not yet; as `right::reverse` and
/// `right::copy_out`.
pub(super) fn copy_out(
    space: &Space,
    port: Reference<Port>,
    message_type: MessageType,
) -> Result<PortName, (Error, Reference<Port>)> {
    let mut state = space.state.write();
    assert!(state.active, "ipc_object_copyout: a dead space, not yet");
    // A space holds each send-once right under a name of its own.
    let reverse = !matches!(message_type, MessageType::PortSendOnce);
    let (name, port_state) = if let Some(found) = reverse
        .then(|| right::reverse(space, &state, &port))
        .flatten()
    {
        found
    } else {
        let name = match state.allocate() {
            Ok((name, _)) => name,
            Err(error) => return Err((error, port)),
        };
        let port_state = port.state.lock();
        assert!(
            port_state.active,
            "ipc_object_copyout: a dead port, not yet"
        );
        (name, port_state)
    };
    // The entry takes the right's reference, unless the right merged into
    // one it holds.
    if let Some(entry) = right::copy_out(&mut state, name, message_type, &port, port_state) {
        entry.object = Some(Object::Port(port));
    } else {
        drop(port);
    }
    drop(state);
    Ok(name)
}

/// Puts the right `message_type` names, for `port`, in `space` as `name`;
/// the space takes the right and its reference.
///
/// # Errors
///
/// `ResourceShortage` if no entry or map node can be allocated, and
/// `NameExists` if `name` holds rights; each gives the right back.
///
/// # Panics
///
/// If the entry `right::reverse` finds holds neither a send nor a receive
/// right. If `port` is dead, or `space` holds a send or receive right for
/// it under another name: not yet; as `SpaceState::allocate_name`,
/// `right::reverse` and `right::copy_out`.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the port's guard is moved into right::copy_out"
)]
pub(super) fn copy_out_name(
    space: &Space,
    port: Reference<Port>,
    message_type: MessageType,
    name: PortName,
) -> Result<(), (Error, Reference<Port>)> {
    let mut state = space.state.write();
    let used = match state.allocate_name(name) {
        Ok(entry) => entry.in_use(),
        Err(error) => return Err((error, port)),
    };
    let port_state = if let Some((found_name, port_state)) =
        (!matches!(message_type, MessageType::PortSendOnce))
            .then(|| right::reverse(space, &state, &port))
            .flatten()
    {
        assert!(
            found_name == name,
            "ipc_object_copyout_name: KERN_RIGHT_EXISTS, not yet"
        );
        assert!(
            state
                .lookup(name)
                .is_some_and(|entry| entry.bits.rights.intersects(Rights::SEND | Rights::RECEIVE)),
            "ipc_object_copyout_name: an entry with no send or receive right"
        );
        port_state
    } else {
        if used {
            return Err((Error::NameExists, port));
        }
        let port_state = port.state.lock();
        assert!(
            port_state.active,
            "ipc_object_copyout_name: a dead port, not yet"
        );
        port_state
    };
    // The entry takes the right's reference, unless the right merged into
    // one it holds.
    if let Some(entry) = right::copy_out(&mut state, name, message_type, &port, port_state) {
        entry.object = Some(Object::Port(port));
    } else {
        drop(port);
    }
    drop(state);
    Ok(())
}

/// Moves the rights of `old_name` in `space` to `new_name`, as
/// `ipc_object_rename` does.
///
/// # Errors
///
/// `ResourceShortage` if the new entry or a map node can't be allocated.
///
/// # Panics
///
/// If `new_name` holds rights, or `old_name` is `new_name` or holds none:
/// not yet. As `SpaceState::allocate_name` and `right::rename`.
pub(super) fn rename(space: &Space, old_name: PortName, new_name: PortName) -> Result<(), Error> {
    let mut state = space.state.write();
    // As ipc_right_inuse.
    let used = state.allocate_name(new_name)?.in_use();
    assert!(!used, "ipc_object_rename: KERN_NAME_EXISTS, not yet");
    assert!(
        old_name != new_name && state.lookup(old_name).is_some(),
        "ipc_object_rename: KERN_INVALID_NAME, not yet"
    );
    right::rename(&mut state, old_name, new_name);
    drop(state);
    Ok(())
}
