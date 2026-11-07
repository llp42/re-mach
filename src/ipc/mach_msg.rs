// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The message trap.

use core::ptr::{self, NonNull};

use super::entry::Rights;
use super::kmsg;
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

/// A request's header bits as the user sends them: a copied send right
/// for its destination, and a send-once right made for its reply port.
const REQUEST_BITS: u32 = 0x1513;

/// A request's header bits once copied in: a send right for its
/// destination, and a send-once right for its reply port.
const COPIED_REQUEST_BITS: u32 = 0x1211;

/// Sends the message at `message`, `send_size` bytes, then receives one on
/// `receive_name`, as `mach_msg_trap` does; so far a request to a port the
/// kernel receives on, from `fast_copyin` to `kernel_send`.
///
/// # Panics
///
/// If no thread runs on the CPU. On every other path: not yet; as
/// `kmsg::get` and `ipc_kobject::server`.
pub fn trap(
    message: usize,
    option: u32,
    send_size: u32,
    _receive_size: u32,
    receive_name: u32,
    _timeout: u32,
    _notify: u32,
) -> u32 {
    assert!(
        option == (Options::SEND | Options::RECEIVE).bits(),
        "mach_msg_trap: option {option:#x}, not yet"
    );
    let thread = smp::current()
        .active_thread()
        .expect("mach_msg_trap: no current thread");
    let space = &thread.task.space;
    // With no cached buffer, fast_get goes to slow_get.
    let mut kmsg = kmsg::get(message, send_size);
    // fast_copyin.
    let header = kmsg.header_mut();
    assert!(
        header.bits == REQUEST_BITS,
        "mach_msg_trap: header bits {:#x}, not yet",
        header.bits
    );
    let reply_name = u32::try_from(header.local_port).unwrap();
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
        .lookup(u32::try_from(header.remote_port).unwrap())
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
    header.bits = COPIED_REQUEST_BITS;
    header.remote_port = ptr::from_ref(destination_port).expose_provenance();
    header.local_port = ptr::from_ref(reply_port).expose_provenance();
    let kernel = destination_state.receiver == NonNull::from(&KERNEL_SPACE);
    drop(reply_state);
    drop(destination_state);
    assert!(kernel, "mach_msg_trap: fast_send_receive, not yet");
    // kernel_send.
    ipc_kobject::server(kmsg)
}
