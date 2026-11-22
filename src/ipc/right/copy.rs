// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::ipc::MessageType;
use crate::ipc::entry::{Entry, Rights};
use crate::ipc::object::Object;
use crate::ipc::port::port::Port;
use crate::ipc::port::port_name::PortName;
use crate::ipc::space::SpaceState;
use crate::reference::Reference;

impl SpaceState {
    /// Copies a send right from the entry of `name`, in the write-locked,
    /// alive space, as `ipc_right_copyin` does for
    /// `MACH_MSG_TYPE_COPY_SEND`: gives the port's new send right with its
    /// reference, the entry unchanged; or none if the port died, the entry
    /// turned into a dead name (`copy_dead`).
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, or its entry has no port. If
    /// the entry is a dead name, or holds no send right: not yet. As
    /// `make_dead_name`.
    pub(in crate::ipc) fn copy_send(&mut self, name: PortName) -> Option<Reference<Port>> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        let rights = entry.bits.rights;
        assert!(
            !rights.contains(Rights::DEAD_NAME),
            "ipc_right_copyin: copy_dead, not yet"
        );
        // Dead send-once rights pass here, for `live_port`.
        assert!(
            rights.intersects(Rights::SEND | Rights::SEND_ONCE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        let Some(Object::Port(port)) = &entry.object else {
            panic!("ipc_right_copyin: a send right with no port");
        };
        let Some(mut port_state) = self.live_port(entry) else {
            self.make_dead_name(name);
            // copy_dead.
            return None;
        };
        assert!(
            rights.contains(Rights::SEND),
            "ipc_right_copyin: a send-once right, KERN_INVALID_RIGHT, not yet"
        );
        port_state.send_rights += 1;
        let port = port.clone();
        drop(port_state);
        Some(port)
    }

    /// Whether a copy-in of `message_type` from `entry`, in the locked, alive
    /// space, would succeed, as `ipc_right_copyin_check`
    /// tells; so far for `MACH_MSG_TYPE_MAKE_SEND_ONCE`: the entry holds a
    /// receive right.
    ///
    /// # Panics
    ///
    /// If the space is dead. For another type: not yet.
    pub(in crate::ipc) fn can_copy_in(&self, entry: &Entry, message_type: MessageType) -> bool {
        assert!(self.active, "ipc_right_copyin_check: a dead space");
        match message_type {
            MessageType::MakeSendOnce => entry.bits.rights.contains(Rights::RECEIVE),
            _ => panic!(
                "ipc_right_copyin_check: type {}, not yet",
                message_type.number()
            ),
        }
    }
}
