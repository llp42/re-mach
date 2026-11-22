// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::ipc::entry::Rights;
use crate::ipc::object::Object;
use crate::ipc::port::port::Port;
use crate::ipc::port::port_name::PortName;
use crate::ipc::space::SpaceState;
use crate::reference::Reference;

impl SpaceState {
    /// Moves a send right out of `name`'s entry, in the write-locked, alive
    /// space, as `ipc_right_copyin` does for
    /// `MACH_MSG_TYPE_MOVE_SEND`: gives the right with its reference. The
    /// entry's last user reference gives the entry's own, leaving the entry no
    /// rights, out of the reverse map, for the caller to free; another makes
    /// the port a new send right and reference.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, the entry has no user
    /// references or no port, or its port no send right. If the entry is a dead
    /// name or holds no send right, its port died, or its last user reference
    /// is also a receive right's or has a dead-name or message-accepted
    /// request: not yet.
    pub(in crate::ipc) fn move_send(&mut self, name: PortName) -> Reference<Port> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        let rights = entry.bits.rights;
        assert!(
            !rights.contains(Rights::DEAD_NAME),
            "ipc_right_copyin: move_dead, not yet"
        );
        // Dead send-once rights pass here, for `live_port`.
        assert!(
            rights.intersects(Rights::SEND | Rights::SEND_ONCE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        assert!(
            entry.bits.user_references > 0,
            "ipc_right_copyin: no user references"
        );
        let Some(mut port_state) = self.live_port(entry) else {
            panic!("ipc_right_check: the port died, its entry to a dead name, move_dead, not yet");
        };
        assert!(
            rights.contains(Rights::SEND),
            "ipc_right_copyin: a send-once right, KERN_INVALID_RIGHT, not yet"
        );
        assert!(
            port_state.send_rights > 0,
            "ipc_right_copyin: a port with no send right"
        );
        if entry.bits.user_references == 1 {
            assert!(
                !rights.contains(Rights::RECEIVE),
                "ipc_right_copyin: a send and receive right's last user reference, not yet"
            );
            // ipc_right_dncancel_macro.
            assert!(entry.request == 0, "ipc_right_dncancel, not yet");
            assert!(
                !entry.bits.message_accepted,
                "ipc_right_copyin: ipc_marequest_cancel, not yet"
            );
            drop(port_state);
            let Self {
                map, reverse_map, ..
            } = self;
            let entry = map.get_mut(name.key()).expect("ipc_right_copyin: no entry");
            let Some(Object::Port(port)) = entry.object.take() else {
                panic!("ipc_right_copyin: a send right with no port");
            };
            reverse_map.remove(port.reverse_key());
            entry.bits.rights.remove(Rights::SEND);
            entry.bits.user_references = 0;
            port
        } else {
            port_state.send_rights += 1;
            let Some(Object::Port(port)) = &entry.object else {
                panic!("ipc_right_copyin: a send right with no port");
            };
            let port = port.clone();
            drop(port_state);
            self.map
                .get_mut(name.key())
                .expect("ipc_right_copyin: no entry")
                .bits
                .user_references -= 1;
            port
        }
    }

    /// Moves the send-once right of `name`'s entry out of the write-locked,
    /// alive space, as `ipc_right_copyin` does for
    /// `MACH_MSG_TYPE_MOVE_SEND_ONCE`: gives the right with the entry's
    /// reference; the entry is left with no rights, for the caller to free.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, or the entry has no user
    /// references or no port. If the entry is a dead name, holds no send-once
    /// right, its port died, or it has a dead-name request: not yet. If a
    /// send-once right's entry holds other rights, other user references or a
    /// message-accepted request, or its port has no send-once right.
    pub(in crate::ipc) fn move_send_once(&mut self, name: PortName) -> Reference<Port> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        let rights = entry.bits.rights;
        assert!(
            !rights.contains(Rights::DEAD_NAME),
            "ipc_right_copyin: move_dead, not yet"
        );
        // Dead send rights pass here, for `live_port`.
        assert!(
            rights.intersects(Rights::SEND | Rights::SEND_ONCE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        assert!(
            entry.bits.user_references > 0,
            "ipc_right_copyin: no user references"
        );
        let Some(port_state) = self.live_port(entry) else {
            panic!("ipc_right_check: the port died, its entry to a dead name, move_dead, not yet");
        };
        assert!(
            rights.contains(Rights::SEND_ONCE),
            "ipc_right_copyin: a send right, KERN_INVALID_RIGHT, not yet"
        );
        assert!(
            rights.bits() == Rights::SEND_ONCE.bits()
                && entry.bits.user_references == 1
                && !entry.bits.message_accepted,
            "ipc_right_copyin: a send-once right's rights, user references or message-accepted request"
        );
        assert!(
            port_state.send_once_rights > 0,
            "ipc_right_copyin: a port with no send-once right"
        );
        // ipc_right_dncancel_macro.
        assert!(entry.request == 0, "ipc_right_dncancel, not yet");
        drop(port_state);
        let entry = self
            .map
            .get_mut(name.key())
            .expect("ipc_right_copyin: no entry");
        let Some(Object::Port(port)) = entry.object.take() else {
            panic!("ipc_right_copyin: a send-once right with no port");
        };
        entry.bits.rights.remove(Rights::SEND_ONCE);
        port
    }
}
