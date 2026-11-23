// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use super::port::Port;
use super::port_name::PortName;
use crate::ipc::MessageType;
use crate::ipc::space::Space;
use crate::reference::Reference;
impl Port {
    /// Readies its receive right to be handed over, as
    /// `ipc_port_clear_receiver` does but for the port to live on.
    ///
    /// # Panics
    ///
    /// As `PortState::clear_receiver_limbo`.
    pub fn clear_receiver_limbo(&self) {
        self.state.lock().clear_receiver_limbo();
    }

    /// Makes a send right for `port` from its receive right, whose
    /// reference the caller lends; gives the right's reference.
    pub fn make_send(port: &Reference<Self>) -> Reference<Self> {
        let mut state = port.state.lock();
        state.make_send_count += 1;
        state.send_rights += 1;
        drop(state);
        port.clone()
    }

    /// Makes a send-once right for `port` from its receive right, whose
    /// reference the caller lends, as `ipc_port_make_sonce` does; gives the
    /// right's reference.
    ///
    /// # Panics
    ///
    /// If `port` is dead: not yet.
    pub fn make_send_once(port: &Reference<Self>) -> Reference<Self> {
        let mut state = port.state.lock();
        assert!(state.active, "ipc_port_make_sonce: a dead port, not yet");
        state.send_once_rights += 1;
        drop(state);
        port.clone()
    }

    /// Makes a send right for `port` from another, whose reference the
    /// caller lends, as `ipc_port_copy_send` does; gives the new right's
    /// reference.
    ///
    /// # Panics
    ///
    /// If `port` has no send right. If it is dead: not yet.
    pub fn copy_send(port: &Reference<Self>) -> Reference<Self> {
        let mut state = port.state.lock();
        assert!(state.active, "ipc_port_copy_send: a dead port, not yet");
        assert!(state.send_rights > 0, "ipc_port_copy_send: no send right");
        let send = port.clone();
        state.send_rights += 1;
        drop(state);
        send
    }

    /// Makes a send right for `port` from another, whose reference the
    /// caller lends, as `ipc_port_copy_send` does: gives the new right's
    /// reference, or none for a dead port.
    ///
    /// # Panics
    ///
    /// If `port` is alive and has no send right.
    pub fn try_copy_send(port: &Reference<Self>) -> Option<Reference<Self>> {
        let mut state = port.state.lock();
        let send = state.active.then(|| {
            assert!(state.send_rights > 0, "ipc_port_copy_send: no send right");
            state.send_rights += 1;
            port.clone()
        });
        drop(state);
        send
    }

    /// Releases a send right for `port`, which the caller gives with its
    /// reference, as `ipc_port_release_send` does: a dead port keeps no
    /// count.
    ///
    /// # Panics
    ///
    /// If `port` is alive and has no send right.
    pub fn release_send(port: Reference<Self>) {
        let mut state = port.state.lock();
        if state.active {
            state.send_rights = state
                .send_rights
                .checked_sub(1)
                .expect("ipc_port_release_send: no send right");
            // With no no-senders request, the last sender's release does no
            // more.
        }
        drop(state);
        drop(port);
    }

    /// Releases a send-once right for `port`, which the caller gives with
    /// its reference, as `ipc_port_release_sonce` does.
    ///
    /// # Panics
    ///
    /// If `port` has no send-once right.
    pub fn release_send_once(port: Reference<Self>) {
        let mut state = port.state.lock();
        state.send_once_rights = state
            .send_once_rights
            .checked_sub(1)
            .expect("ipc_port_release_sonce: no send-once right");
        drop(state);
        drop(port);
    }

    /// Puts a send right for `port`, which the caller gives with its
    /// reference, in `space`, as `ipc_port_copyout_send` does; gives its
    /// name there.
    ///
    /// # Panics
    ///
    /// If the copy-out fails: not yet; as `Space::copy_out`.
    pub fn copy_out_send(port: Reference<Self>, space: &Space) -> PortName {
        space
            .copy_out(port, MessageType::PortSend)
            .unwrap_or_else(|(error, _)| {
                panic!("ipc_port_copyout_send: a failed copy-out, not yet: {error:?}")
            })
    }
}
