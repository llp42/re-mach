// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;

use crate::ipc::MessageType;
use crate::ipc::entry::{Entry, Rights};
use crate::ipc::object::Object;
use crate::ipc::port::port::{Port, Receiver};
use crate::ipc::port::port_name::PortName;
use crate::ipc::port::state::PortState;
use crate::ipc::space::{Space, SpaceState};
use crate::reference::Reference;
use crate::sync::SpinlockGuard;

impl SpaceState {
    /// Puts the right `message_type` names, for `port`, of state `port_state`,
    /// in the entry of `name`: the entry takes the right, or a user reference
    /// for a right it holds; the port is unlocked.
    /// Gives the entry if it is to take the right's reference, none if it keeps
    /// its own and the caller drops the right's.
    ///
    /// # Panics
    ///
    /// If `name` has no entry, its entry holds the receive right with other
    /// rights or user references, or a send-once right's entry is in use or
    /// the port has none. If its user references would reach the maximum: not
    /// yet. For a type that is not a right.
    pub(in crate::ipc) fn put_right(
        &mut self,
        name: PortName,
        message_type: MessageType,
        port: &Port,
        mut port_state: SpinlockGuard<'_, PortState>,
    ) -> Option<&mut Entry> {
        match message_type {
            MessageType::PortSend => {
                let Self {
                    map, reverse_map, ..
                } = self;
                // The name map's own pointer, for the reverse map, and its entry.
                let (Some(pointer), Some(entry)) =
                    (map.lookup(name.key()), map.get_mut(name.key()))
                else {
                    panic!("ipc_right_copyout: no entry");
                };
                let bits = &entry.bits;
                let takes = if bits.rights.contains(Rights::SEND) {
                    assert!(
                        bits.user_references + 1 != u16::MAX,
                        "ipc_right_copyout: MACH_PORT_UREFS_MAX, not yet"
                    );
                    // It merges into the entry's right, which holds a reference.
                    port_state.send_rights -= 1;
                    drop(port_state);
                    false
                } else if bits.rights.contains(Rights::RECEIVE) {
                    assert!(
                        bits.rights.bits() == Rights::RECEIVE.bits() && bits.user_references == 0,
                        "ipc_right_copyout: a receive right's entry with other rights or user references"
                    );
                    // The send right moves to the entry; the entry keeps its own reference.
                    drop(port_state);
                    false
                } else {
                    drop(port_state);
                    // As in GNU Mach, a failure leaves the entry out of the reverse map.
                    let _ = reverse_map.insert(port.reverse_key(), pointer);
                    entry.name = name;
                    true
                };
                entry.bits.rights |= Rights::SEND;
                entry.bits.user_references += 1;
                takes.then_some(entry)
            }
            MessageType::PortSendOnce => {
                let entry = self
                    .map
                    .get_mut(name.key())
                    .expect("ipc_right_copyout: no entry");
                assert!(
                    !entry.in_use() && port_state.send_once_rights > 0,
                    "ipc_right_copyout: a send-once right to an entry in use, or none"
                );
                drop(port_state);
                // The entry takes the right and its reference.
                entry.bits.rights |= Rights::SEND_ONCE;
                entry.bits.user_references = 1;
                Some(entry)
            }
            _ => panic!("ipc_right_copyout: strange rights"),
        }
    }

    /// Makes a send right from the receive right of `name`'s entry, in the
    /// write-locked, alive `space`, whose state it is, as `ipc_right_copyin`
    /// does for `MACH_MSG_TYPE_MAKE_SEND`: gives the port's new send right with
    /// its reference; the entry is unchanged.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, or its receive right has no
    /// port, a dead one or one received under another name or space. If the
    /// entry holds no receive right: not yet.
    pub(in crate::ipc) fn make_send(&self, space: &Space, name: PortName) -> Reference<Port> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        assert!(
            entry.bits.rights.contains(Rights::RECEIVE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        let Some(Object::Port(port)) = &entry.object else {
            panic!("ipc_right_copyin: a receive right with no port");
        };
        let mut port_state = port.state.lock();
        assert!(
            port_state.active,
            "ipc_right_copyin: a dead port's receive right"
        );
        assert!(
            port_state.receiver_name == name
                && port_state.receiver == Receiver::Space(NonNull::from(space)),
            "ipc_right_copyin: a receive right under another name or space"
        );
        port_state.make_send_count += 1;
        port_state.send_rights += 1;
        let port = port.clone();
        drop(port_state);
        port
    }

    /// Makes a send-once right from the receive right of `name`'s entry, in the
    /// write-locked, alive `space`, whose state it is, as `ipc_right_copyin`
    /// does for `MACH_MSG_TYPE_MAKE_SEND_ONCE`: gives the port's new send-once
    /// right with its reference; the entry is unchanged.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, or its receive right has no
    /// port, a dead one or one received under another name or space. If the
    /// entry holds no receive right: not yet.
    pub(in crate::ipc) fn make_send_once(&self, space: &Space, name: PortName) -> Reference<Port> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        assert!(
            entry.bits.rights.contains(Rights::RECEIVE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        let Some(Object::Port(port)) = &entry.object else {
            panic!("ipc_right_copyin: a receive right with no port");
        };
        let mut port_state = port.state.lock();
        assert!(
            port_state.active,
            "ipc_right_copyin: a dead port's receive right"
        );
        assert!(
            port_state.receiver_name == name
                && port_state.receiver == Receiver::Space(NonNull::from(space)),
            "ipc_right_copyin: a receive right under another name or space"
        );
        port_state.send_once_rights += 1;
        let port = port.clone();
        drop(port_state);
        port
    }

    /// Turns `name`'s send right, whose port died, into a dead name in the
    /// write-locked, alive space, as `ipc_right_check` does
    /// once it finds the port dead: the entry keeps its user references and
    /// releases the port's reference, which the reverse map no longer holds.
    ///
    /// # Panics
    ///
    /// If `name` has no entry, no user references, or rights other than a send
    /// or send-once right, or the entry has no port. For a send-once right, or
    /// a message-accepted or dead-name request: not yet.
    pub(super) fn make_dead_name(&mut self, name: PortName) {
        let Self {
            map, reverse_map, ..
        } = self;
        let entry = map.get_mut(name.key()).expect("ipc_right_check: no entry");
        let rights = entry.bits.rights.bits();
        assert!(
            entry.bits.user_references > 0,
            "ipc_right_check: no user references"
        );
        assert!(
            rights != Rights::SEND_ONCE.bits(),
            "ipc_right_check: a send-once right, not yet"
        );
        assert!(
            rights == Rights::SEND.bits(),
            "ipc_right_check: neither a send nor a send-once right"
        );
        assert!(
            !entry.bits.message_accepted,
            "ipc_right_check: ipc_marequest_cancel, not yet"
        );
        let Some(Object::Port(port)) = entry.object.take() else {
            panic!("ipc_right_check: a send right with no port");
        };
        reverse_map.remove(port.reverse_key());
        // As ipc_port_release: the entry's reference, the last frees the port.
        drop(port);
        entry.bits.rights = Rights::DEAD_NAME;
        // No call makes a dead-name request yet: none adds a user reference.
        assert!(
            entry.request == 0,
            "ipc_right_check: a dead-name request, not yet"
        );
    }
}
