// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;

use super::port::{Port, Receiver};
use super::port_name::PortName;
use crate::ipc::space;
use crate::reference::Reference;
impl Port {
    /// Gives up `port`, the reply port of a thread that ends, with the
    /// thread's reference, which drops (SC-D2): with send-once rights out of
    /// its queue, it is orphaned, destroyed once they all sit there.
    ///
    /// # Panics
    ///
    /// If it is dead, not a reply port, or has send rights. With no
    /// send-once right out of its queue: not yet.
    pub fn orphan_reply(port: Reference<Self>) {
        let mut state = port.state.lock();
        assert!(
            state.active
                && state.receiver == Receiver::Space(NonNull::from(&space::REPLY_SPACE))
                && state.send_rights == 0,
            "orphan_reply: not a live reply port, or one with send rights"
        );
        assert!(
            !state.send_once_rights_queued(),
            "orphan_reply: ipc_port_dealloc_reply, not yet"
        );
        state.orphaned = true;
        drop(state);
        drop(port);
    }

    /// Destroys the port, which receives kernel replies, as
    /// `ipc_port_dealloc_reply` does: its receiver goes, then it is
    /// destroyed. The caller then drops the reference GNU Mach's call
    /// consumes.
    ///
    /// # Panics
    ///
    /// If it is dead or not a reply port. As `destroy`.
    pub(in crate::ipc) fn deallocate_reply(&self) {
        let mut state = self.state.lock();
        assert!(state.active, "ipc_port_dealloc_special: a dead port");
        assert!(
            state.receiver_name != PortName::NULL
                && state.receiver == Receiver::Space(NonNull::from(&space::REPLY_SPACE)),
            "ipc_port_dealloc_special: not the reply space's"
        );
        // Its receiver goes; `destroy` marks it dead.
        state.receiver_name = PortName::NULL;
        state.clear_receiver();
        self.destroy(state);
    }
}
