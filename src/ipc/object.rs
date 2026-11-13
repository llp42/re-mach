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
    let found = if matches!(message_type, MessageType::PortSendOnce) {
        None
    } else {
        right::reverse(space, &state, port)
    };
    let (name, port_state) = if let Some(found) = found {
        found
    } else {
        let (name, entry) = state.allocate()?;
        let port_state = port.state.lock();
        assert!(
            port_state.active,
            "ipc_object_copyout: a dead port, not yet"
        );
        entry.object = Some(NonNull::from(port));
        (name, port_state)
    };
    right::copy_out(&mut state, name, message_type, port, port_state);
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
/// If `port` is dead, or `space` holds a send right for it: not yet; as
/// `SpaceState::allocate_name`, `right::reverse` and `right::copy_out`.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the port's guard is moved into right::copy_out"
)]
pub(super) fn copy_out_name(
    space: &Space,
    port: &Port,
    message_type: MessageType,
    name: PortName,
) -> Result<(), Error> {
    let mut state = space.state.write();
    let used = state.allocate_name(name)?.in_use();
    if !matches!(message_type, MessageType::PortSendOnce) {
        let found = right::reverse(space, &state, port);
        assert!(
            found.is_none(),
            "ipc_object_copyout_name: a right ipc_right_reverse found, not yet"
        );
    }
    if used {
        return Err(Error::NameExists);
    }
    let entry = state
        .map
        .get_mut(name.key())
        .expect("ipc_object_copyout_name: no entry");
    let port_state = port.state.lock();
    assert!(
        port_state.active,
        "ipc_object_copyout_name: a dead port, not yet"
    );
    entry.object = Some(NonNull::from(port));
    right::copy_out(&mut state, name, message_type, port, port_state);
    drop(state);
    Ok(())
}
