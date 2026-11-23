// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::error::Error;
use crate::ipc::entry::Rights;
use crate::ipc::object::Object;
use crate::ipc::port::port::Port;
use crate::ipc::port::port_name::{PortName, Right};
use crate::ipc::space::{Space, SpaceState};

impl Space {
    /// Modifies `name`'s user references of the right numbered `right` by
    /// `delta`, as `mach_port_mod_refs` does; a right whose references reach
    /// zero is released.
    ///
    /// # Errors
    ///
    /// `InvalidValue` for a kind of right that cannot change that way,
    /// `InvalidRight` if `name` holds no right of that kind, and
    /// `UrefsOverflow` if the count would pass its maximum.
    ///
    /// # Panics
    ///
    /// If `name` holds no rights, or a send-once, receive and send, or other
    /// right whose release is not written here: not yet. As `Space::lookup_write`,
    /// `SpaceState::release_user_reference` and `SpaceState::destroy_rights`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the space's guard is held while its entry is changed"
    )]
    #[expect(
        clippy::too_many_lines,
        reason = "one arm per right kind, as ipc_right_delta"
    )]
    pub fn mod_refs(&self, name: PortName, right: u32, delta: i32) -> Result<(), Error> {
        let right = Right::try_from(right).map_err(|_| Error::InvalidValue)?;
        let state = self.lookup_write(name);
        let (rights, user_references) = {
            let entry = state
                .map
                .get(name.key())
                .expect("ipc_right_delta: no entry");
            (entry.bits.rights, entry.bits.user_references)
        };
        let held = match right {
            Right::Send => Rights::SEND,
            Right::Receive => Rights::RECEIVE,
            Right::SendOnce => Rights::SEND_ONCE,
            Right::PortSet => Rights::PORT_SET,
            Right::DeadName => Rights::DEAD_NAME,
        };
        if !rights.contains(held) {
            drop(state);
            return Err(Error::InvalidRight);
        }
        match right {
            Right::Receive => {
                if delta == 0 {
                    drop(state);
                    return Ok(());
                }
                if delta != -1 {
                    drop(state);
                    return Err(Error::InvalidValue);
                }
                if rights.bits() == (Rights::SEND | Rights::RECEIVE).bits() {
                    SpaceState::destroy_send_receive(state, self, name);
                    return Ok(());
                }
                if rights.bits() != Rights::RECEIVE.bits() {
                    drop(state);
                    return Err(Error::InvalidValue);
                }
                SpaceState::destroy_rights(state, self, name);
                Ok(())
            }
            Right::PortSet => {
                if delta == 0 {
                    drop(state);
                    return Ok(());
                }
                if delta != -1 || rights.bits() != Rights::PORT_SET.bits() {
                    drop(state);
                    return Err(Error::InvalidValue);
                }
                SpaceState::destroy_rights(state, self, name);
                Ok(())
            }
            Right::SendOnce => {
                if delta == 0 {
                    drop(state);
                    return Ok(());
                }
                if delta != -1 {
                    drop(state);
                    return Err(Error::InvalidValue);
                }
                assert!(
                    user_references == 1,
                    "ipc_right_delta: a send-once right with no user reference"
                );
                let mut state = state;
                let port = {
                    let entry = state
                        .map
                        .get_mut(name.key())
                        .expect("ipc_right_delta: no entry");
                    let Some(Object::Port(port)) = entry.object.take() else {
                        panic!("ipc_right_delta: a send-once right with no port");
                    };
                    port
                };
                state.deallocate(name);
                drop(state);
                Port::release_send_once(port);
                Ok(())
            }
            Right::DeadName => {
                let count = i32::from(user_references)
                    .checked_add(delta)
                    .ok_or(Error::UrefsOverflow)?;
                if count < 0 {
                    drop(state);
                    return Err(Error::InvalidValue);
                }
                if count == 0 {
                    let mut state = state;
                    state.deallocate(name);
                    drop(state);
                } else {
                    let count = u16::try_from(count).map_err(|_| Error::UrefsOverflow)?;
                    let mut state = state;
                    state
                        .map
                        .get_mut(name.key())
                        .expect("ipc_right_delta: no entry")
                        .bits
                        .user_references = count;
                    drop(state);
                }
                Ok(())
            }
            Right::Send => {
                let count = i32::from(user_references)
                    .checked_add(delta)
                    .ok_or(Error::UrefsOverflow)?;
                if count < 0 {
                    drop(state);
                    return Err(Error::InvalidValue);
                }
                if count == 0 {
                    // Keep one reference for the release to take, which frees
                    // the port's send right.
                    let mut state = state;
                    state
                        .map
                        .get_mut(name.key())
                        .expect("ipc_right_delta: no entry")
                        .bits
                        .user_references = 1;
                    SpaceState::release_user_reference(state, name);
                } else {
                    let count = u16::try_from(count).map_err(|_| Error::UrefsOverflow)?;
                    let mut state = state;
                    state
                        .map
                        .get_mut(name.key())
                        .expect("ipc_right_delta: no entry")
                        .bits
                        .user_references = count;
                    drop(state);
                }
                Ok(())
            }
        }
    }
}
