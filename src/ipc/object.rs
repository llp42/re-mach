// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Objects: what a space's names stand for; so far ports.

use core::ptr::NonNull;

use super::entry::Rights;
use super::port::{Port, PortName};
use super::space::Space;
use super::{MessageType, right};
use crate::error::Error;
use crate::slab::CacheBox;

/// Puts the receive right for `port`, new, in `space` under the lowest
/// free name, and gives the name; the entry takes the right and the
/// port's reference, linked only once the port knows the name.
///
/// # Errors
///
/// `ResourceShortage` if no entry or map node can be allocated; the port
/// is freed.
///
/// # Panics
///
/// As `SpaceState::allocate`.
pub(super) fn allocate(space: &Space, mut port: CacheBox<Port>) -> Result<PortName, Error> {
    let mut state = space.state.write();
    let (name, entry) = state.allocate()?;
    port.state.get_mut().receiver_name = name;
    let port = CacheBox::into_raw(port);
    // SAFETY: the space's lock, held, guards its entries, and no reference
    // to this new one is alive.
    let entry = unsafe { &mut *entry.as_ptr() };
    // The receive right, with no user references.
    entry.bits.rights |= Rights::RECEIVE;
    entry.object = Some(port);
    drop(state);
    Ok(name)
}

/// Puts the right `message_type` names, for `port`, in `space` under the
/// lowest free name, as `ipc_object_copyout` does, and gives the name; the
/// space takes the right and its reference.
///
/// # Errors
///
/// `ResourceShortage` if no entry or map node can be allocated.
///
/// # Panics
///
/// If `space` or `port` is dead: not yet; as `right::reverse` and
/// `right::copy_out`.
pub(super) fn copy_out(
    space: &Space,
    port: &Port,
    message_type: MessageType,
) -> Result<PortName, Error> {
    let mut state = space.state.write();
    assert!(state.active, "ipc_object_copyout: a dead space, not yet");
    // A space holds each send-once right under a name of its own.
    if !matches!(message_type, MessageType::PortSendOnce) {
        right::reverse(space, &state, port);
    }
    let (name, entry) = state.allocate()?;
    let port_state = port.state.lock();
    assert!(
        port_state.active,
        "ipc_object_copyout: a dead port, not yet"
    );
    // SAFETY: the space's lock, held, guards its entries, and no reference
    // to this new one is alive.
    unsafe { (*entry.as_ptr()).object = Some(NonNull::from(port)) };
    // SAFETY: `allocate` gave the space's entry for `name`, which the lock
    // still guards, and no reference to it is alive.
    unsafe { right::copy_out(&mut state, name, entry, message_type, port, port_state) };
    drop(state);
    Ok(name)
}

/// Puts the right `message_type` names, for `port`, in `space` as `name`;
/// the space takes the right and its reference.
///
/// # Errors
///
/// `ResourceShortage` if no entry or map node can be allocated, and
/// `NameExists` if `name` holds rights.
///
/// # Panics
///
/// If `port` is dead: not yet; as `SpaceState::allocate_name`,
/// `right::reverse` and `right::copy_out`.
pub(super) fn copy_out_name(
    space: &Space,
    port: &Port,
    message_type: MessageType,
    name: PortName,
) -> Result<(), Error> {
    let mut state = space.state.write();
    let entry = state.allocate_name(name)?;
    if !matches!(message_type, MessageType::PortSendOnce) {
        right::reverse(space, &state, port);
    }
    // SAFETY: the space's lock, held, guards its entries, and no reference
    // to this one is alive.
    if unsafe { entry.as_ref() }.in_use() {
        return Err(Error::NameExists);
    }
    let port_state = port.state.lock();
    assert!(
        port_state.active,
        "ipc_object_copyout_name: a dead port, not yet"
    );
    // SAFETY: as above; a free entry is a new one, never the zero entry.
    unsafe { (*entry.as_ptr()).object = Some(NonNull::from(port)) };
    // SAFETY: `allocate_name` gave the space's entry for `name`, which the
    // lock still guards, and no reference to it is alive.
    unsafe { right::copy_out(&mut state, name, entry, message_type, port, port_state) };
    drop(state);
    Ok(())
}
