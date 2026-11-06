// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Objects: what a space's names stand for; so far ports.

use core::ptr::NonNull;

use super::port::Port;
use super::space::Space;
use super::{MessageType, right};
use crate::kernel::Error;

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
    name: u32,
) -> Result<(), Error> {
    let mut state = space.state.write();
    let entry = state.allocate_name(name)?;
    right::reverse(space, &state, port);
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
