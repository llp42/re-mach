// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The message trap.

use core::ptr::{self, NonNull};

use super::MessageType;
use super::entry::Rights;
use super::kernel_message::{self, MessageBits};
use super::port::PortName;
use super::space::KERNEL_SPACE;
use crate::{ipc_kobject, smp};

bitflags::bitflags! {
    /// What a message trap does, as Mach's option bits; only those used
    /// here.
    struct Options: u32 {
        /// Send a message.
        const SEND = 1;
        /// Receive a message.
        const RECEIVE = 1 << 1;
    }
}

/// Sends the message at `message`, `send_size` bytes, then receives one on
/// `receive_name`, as `mach_msg_trap` does; so far a request to a port the
/// kernel receives on, from `fast_copyin` to `kernel_send`.
///
/// # Panics
///
/// If no thread runs on the CPU. On every other path: not yet; as
/// `kernel_message::get` and `ipc_kobject::server`.
pub fn trap(
    message: usize,
    option: u32,
    send_size: u32,
    _receive_size: u32,
    receive_name: PortName,
    _timeout: u32,
    _notify: PortName,
) -> u32 {
    assert!(
        option == (Options::SEND | Options::RECEIVE).bits(),
        "mach_msg_trap: option {option:#x}, not yet"
    );
    let thread = smp::current()
        .active_thread()
        .expect("mach_msg_trap: no current thread");
    let space = &thread.task().space;
    // With no cached buffer, fast_get goes to slow_get.
    let mut message = kernel_message::get(message, send_size);
    // fast_copyin.
    let header = message.header_mut();
    // A copied send right for the destination, and a send-once right made
    // for the reply port.
    assert!(
        header.bits
            == MessageBits::new(Some(MessageType::CopySend), Some(MessageType::MakeSendOnce)),
        "mach_msg_trap: header bits {:#x}, not yet",
        u32::from(header.bits)
    );
    let reply_name = PortName::from(u32::try_from(header.local_port).unwrap());
    assert!(
        reply_name == receive_name,
        "mach_msg_trap: slow_copyin, not yet"
    );
    let state = space.state.read();
    let reply = state
        .lookup(reply_name)
        .filter(|entry| entry.bits.rights.contains(Rights::RECEIVE))
        .unwrap_or_else(|| panic!("mach_msg_trap: slow_copyin, not yet"));
    let destination = state
        .lookup(PortName::from(u32::try_from(header.remote_port).unwrap()))
        .unwrap_or_else(|| panic!("mach_msg_trap: slow_copyin, not yet"));
    assert!(
        destination.bits.rights.bits() == Rights::SEND.bits(),
        "mach_msg_trap: slow_copyin, not yet"
    );
    let (Some(reply_port), Some(destination_port)) = (reply.object, destination.object) else {
        panic!("mach_msg_trap: an entry with no port");
    };
    // SAFETY: the entries hold references to their ports, and the space's
    // lock keeps the entries; then the message holds its own.
    let (reply_port, destination_port) =
        unsafe { (reply_port.as_ref(), destination_port.as_ref()) };
    let mut destination_state = destination_port.state.lock();
    let Some(mut reply_state) = destination_state
        .active
        .then(|| reply_port.state.try_lock())
        .flatten()
    else {
        panic!("mach_msg_trap: slow_copyin, not yet");
    };
    drop(state);
    destination_state.send_rights += 1;
    destination_port.reference();
    reply_state.send_once_rights += 1;
    reply_port.reference();
    header.bits = MessageBits::new(Some(MessageType::PortSend), Some(MessageType::PortSendOnce));
    header.remote_port = ptr::from_ref(destination_port).expose_provenance();
    header.local_port = ptr::from_ref(reply_port).expose_provenance();
    let kernel = destination_state.receiver == NonNull::from(&KERNEL_SPACE);
    drop(reply_state);
    drop(destination_state);
    assert!(kernel, "mach_msg_trap: fast_send_receive, not yet");
    // kernel_send.
    ipc_kobject::server(message)
}
