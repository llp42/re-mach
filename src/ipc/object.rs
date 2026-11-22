// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Objects: what a space's names stand for, ports and port sets.

use core::ptr::NonNull;

use super::MessageType;
use super::entry::Rights;
use super::port::port::{Port, Receiver};
use super::port::port_name::PortName;
use super::port::state::PortState;
use super::port_set::PortSet;
use super::space::Space;
use crate::error::Error;
use crate::reference::Reference;
use crate::slab::CacheBox;
use crate::sync::SpinlockGuard;

/// What an entry's rights are for.
pub(super) enum Object {
    /// A port, by its reference.
    Port(Reference<Port>),
    /// A port set.
    PortSet(NonNull<PortSet>),
}

impl Space {
    /// Puts the right for `object`, new, in it under a free name, as
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
    pub(super) fn insert_object<T>(
        &self,
        mut object: CacheBox<T>,
        set_name: fn(&mut T, PortName),
        kind: fn(CacheBox<T>) -> Object,
    ) -> Result<PortName, Error> {
        let mut state = self.state.write();
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

    /// Puts the right for `object`, new, in it as `name`, as
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
    pub(super) fn insert_object_as<T>(
        &self,
        object: CacheBox<T>,
        name: PortName,
        kind: fn(CacheBox<T>) -> Object,
    ) -> Result<(), Error> {
        let mut state = self.state.write();
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

    /// Puts a dead name, new, in it under a free name, as
    /// `ipc_object_alloc_dead` does, and gives the name.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no entry or map node can be allocated.
    ///
    /// # Panics
    ///
    /// If the entry has an object. As `SpaceState::allocate`.
    pub(super) fn insert_dead_name(&self) -> Result<PortName, Error> {
        let mut state = self.state.write();
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

    /// Puts a dead name, new, in it as `name`, as
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
    pub(super) fn insert_dead_name_as(&self, name: PortName) -> Result<(), Error> {
        let mut state = self.state.write();
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

    /// Copies in the right `message_type` names from `name` in it, as
    /// `ipc_object_copyin` does: gives the port's right with its reference; an
    /// entry left with no rights is freed.
    ///
    /// # Panics
    ///
    /// For a type that is not a right. For a right other than a copied, made
    /// or moved send right, or a copied one whose port died: not yet. As
    /// `Space::lookup_write`, `SpaceState::copy_send`, `SpaceState::make_send`,
    /// `SpaceState::move_send` and `SpaceState::deallocate`.
    pub(super) fn copy_in(&self, name: PortName, message_type: MessageType) -> Reference<Port> {
        let mut state = self.lookup_write(name);
        let port = match message_type {
            MessageType::CopySend => state
                .copy_send(name)
                .unwrap_or_else(|| panic!("ipc_object_copyin: copy_dead, IO_DEAD, not yet")),
            MessageType::MakeSend => state.make_send(self, name),
            MessageType::PortSend => state.move_send(name),
            MessageType::PortSendOnce | MessageType::MakeSendOnce => {
                panic!("ipc_right_copyin: MOVE_SEND_ONCE and MAKE_SEND_ONCE, not yet")
            }
            _ => panic!("ipc_right_copyin: strange rights"),
        };
        if state.lookup(name).is_none() {
            state.deallocate(name);
        }
        drop(state);
        port
    }

    /// Puts the right `message_type` names, for `port`, in it under a free
    /// name, as `ipc_object_copyout` does, and gives the name; it takes the
    /// right and its reference.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no entry or map node can be allocated; it gives
    /// the right back.
    ///
    /// # Panics
    ///
    /// If it or `port` is dead: not yet; as `SpaceState::reverse_lookup` and
    /// `SpaceState::put_right`.
    pub(super) fn copy_out(
        &self,
        port: Reference<Port>,
        message_type: MessageType,
    ) -> Result<PortName, (Error, Reference<Port>)> {
        let mut state = self.state.write();
        assert!(state.active, "ipc_object_copyout: a dead space, not yet");
        // A space holds each send-once right under a name of its own.
        let reverse = !matches!(message_type, MessageType::PortSendOnce);
        let (name, port_state) =
            if let Some(found) = reverse.then(|| state.reverse_lookup(self, &port)).flatten() {
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
        if let Some(entry) = state.put_right(name, message_type, &port, port_state) {
            entry.object = Some(Object::Port(port));
        } else {
            drop(port);
        }
        drop(state);
        Ok(name)
    }

    /// Consumes a message's destination right, of type `message_type`, for a
    /// port of locked state `port_state`, as `ipc_object_copyout_dest` does on
    /// its receive in it: gives its name for the port if it receives on the
    /// port, else null. The port is unlocked; the caller drops the
    /// right's reference.
    ///
    /// # Panics
    ///
    /// If the port is dead or has no right of the type, or for a type that is
    /// not a send or send-once right. If another space receives on a send-once
    /// right's port: not yet.
    pub(super) fn copy_out_destination(
        &self,
        mut port_state: SpinlockGuard<'_, PortState>,
        message_type: MessageType,
    ) -> PortName {
        assert!(port_state.active, "ipc_object_copyout_dest: a dead port");
        let name = match message_type {
            MessageType::PortSend => {
                port_state.send_rights = port_state
                    .send_rights
                    .checked_sub(1)
                    .expect("ipc_object_copyout_dest: no send right");
                // No call makes a no-senders request yet: none to notify.
                if port_state.receiver == Receiver::Space(NonNull::from(self)) {
                    port_state.receiver_name
                } else {
                    PortName::NULL
                }
            }
            MessageType::PortSendOnce => {
                assert!(
                    port_state.send_once_rights > 0,
                    "ipc_object_copyout_dest: no send-once right"
                );
                // Another space receives on it: ipc_notify_send_once.
                assert!(
                    port_state.receiver == Receiver::Space(NonNull::from(self)),
                    "ipc_object_copyout_dest: ipc_notify_send_once, not yet"
                );
                // Quietly consumed.
                port_state.send_once_rights -= 1;
                port_state.receiver_name
            }
            _ => panic!("ipc_object_copyout_dest: strange rights"),
        };
        drop(port_state);
        name
    }

    /// Puts the right `message_type` names, for `port`, in it as `name`; it
    /// takes the right and its reference.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no entry or map node can be allocated, and
    /// `NameExists` if `name` holds rights; each gives the right back.
    ///
    /// # Panics
    ///
    /// If the entry `SpaceState::reverse_lookup` finds holds neither a send nor
    /// a receive right. If `port` is dead, or it holds a send or receive right
    /// for the port under another name: not yet; as
    /// `SpaceState::allocate_name`, `SpaceState::reverse_lookup` and
    /// `SpaceState::put_right`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the port's guard is moved into SpaceState::put_right"
    )]
    pub(super) fn copy_out_as(
        &self,
        port: Reference<Port>,
        message_type: MessageType,
        name: PortName,
    ) -> Result<(), (Error, Reference<Port>)> {
        let mut state = self.state.write();
        let used = match state.allocate_name(name) {
            Ok(entry) => entry.in_use(),
            Err(error) => return Err((error, port)),
        };
        let port_state = if let Some((found_name, port_state)) =
            (!matches!(message_type, MessageType::PortSendOnce))
                .then(|| state.reverse_lookup(self, &port))
                .flatten()
        {
            assert!(
                found_name == name,
                "ipc_object_copyout_name: KERN_RIGHT_EXISTS, not yet"
            );
            assert!(
                state.lookup(name).is_some_and(|entry| entry
                    .bits
                    .rights
                    .intersects(Rights::SEND | Rights::RECEIVE)),
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
        if let Some(entry) = state.put_right(name, message_type, &port, port_state) {
            entry.object = Some(Object::Port(port));
        } else {
            drop(port);
        }
        drop(state);
        Ok(())
    }

    /// Moves the rights of `old_name` in it to `new_name`, as
    /// `ipc_object_rename` does.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the new entry or a map node can't be allocated.
    ///
    /// # Panics
    ///
    /// If `new_name` holds rights, or `old_name` is `new_name` or holds none:
    /// not yet. As `SpaceState::allocate_name` and `SpaceState::move_rights`.
    pub(super) fn rename_entry(&self, old_name: PortName, new_name: PortName) -> Result<(), Error> {
        let mut state = self.state.write();
        // As ipc_right_inuse.
        let used = state.allocate_name(new_name)?.in_use();
        assert!(!used, "ipc_object_rename: KERN_NAME_EXISTS, not yet");
        assert!(
            old_name != new_name && state.lookup(old_name).is_some(),
            "ipc_object_rename: KERN_INVALID_NAME, not yet"
        );
        state.move_rights(old_name, new_name);
        drop(state);
        Ok(())
    }
}
