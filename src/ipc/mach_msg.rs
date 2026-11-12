// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The message trap.

use core::ptr::{self, NonNull};

use super::MessageType;
use super::entry::Rights;
use super::kernel_message::{
    self, EXPAND_FACTOR, Header, KernelMessage, MessageBits, SAVED_MESSAGE_SIZE,
};
use super::port::{Port, PortName};
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
/// kernel receives on, from `fast_get` until its reply is copied out.
///
/// # Panics
///
/// If no thread runs on the CPU, or the reply port has no send-once right.
/// On every other path: not yet; as `kernel_message::get`,
/// `ipc_kobject::server` and `Port::release`. Then, at putting the reply:
/// not yet.
#[expect(
    clippy::too_many_lines,
    reason = "mach_msg_trap's fast path, in GNU Mach's order"
)]
pub fn trap(
    message: usize,
    option: u32,
    send_size: u32,
    receive_size: u32,
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
    // fast_get: the CPU's saved buffer, if the message fits one, else
    // slow_get; a failed copy-in frees it and goes to slow_get too.
    let length = usize::try_from(send_size).unwrap();
    let fits = length * EXPAND_FACTOR <= SAVED_MESSAGE_SIZE
        && length >= size_of::<Header>()
        && length.is_multiple_of(4);
    let mut message = fits
        .then(KernelMessage::take_cached)
        .flatten()
        .and_then(|mut cached| cached.copy_in(message, send_size).is_ok().then_some(cached))
        .unwrap_or_else(|| kernel_message::get(message, send_size));
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
    let mut reply = ipc_kobject::server(message);
    // SAFETY: the reply's destination is the reply port's exposed address,
    // and the reply holds a reference to it.
    let reply_port = unsafe { &*ptr::with_exposed_provenance::<Port>(reply.header().remote_port) };
    let mut reply_state = reply_port.state.lock();
    // Received directly only from a live port this space receives on under
    // `receive_name`, in no set, with no thread waiting and nothing queued.
    assert!(
        reply_state.active
            && reply_state.receiver == NonNull::from(&**space)
            && reply_state.receiver_name == receive_name
            && reply_state.set.is_none(),
        "mach_msg_trap: ipc_mqueue_send_always, not yet"
    );
    assert!(
        reply_state.receivers.is_empty() && reply_state.messages.is_empty(),
        "mach_msg_trap: ipc_mqueue_send_always, not yet"
    );
    reply.header_mut().sequence_number = reply_state.sequence_number;
    reply_state.sequence_number = reply_state.sequence_number.wrapping_add(1);
    drop(reply_state);
    // fast_copyout.
    let reply_size = reply.header().size;
    assert!(
        receive_size >= reply_size,
        "mach_msg_trap: slow_copyout, MACH_RCV_TOO_LARGE, not yet"
    );
    // A reply, with the send-once right it carries.
    assert!(
        reply.header().bits == MessageBits::new(Some(MessageType::PortSendOnce), None),
        "mach_msg_trap: copying out bits {:#x}, not yet",
        u32::from(reply.header().bits)
    );
    let mut reply_state = reply_port.state.lock();
    assert!(
        reply_state.active,
        "mach_msg_trap: slow_copyout, a dead port, not yet"
    );
    let payload = reply_state.protected_payload;
    assert!(
        reply_state.receiver == NonNull::from(&**space),
        "mach_msg_trap: ipc_notify_send_once, not yet"
    );
    reply_port.release();
    reply_state.send_once_rights = reply_state
        .send_once_rights
        .checked_sub(1)
        .expect("mach_msg_trap: no send-once right");
    let name = reply_state.receiver_name;
    drop(reply_state);
    assert!(
        payload.is_none(),
        "mach_msg_trap: a protected payload, not yet"
    );
    let header = reply.header_mut();
    header.bits = MessageBits::new(None, Some(MessageType::PortSendOnce));
    header.local_port = usize::try_from(u32::from(name)).unwrap();
    header.remote_port = usize::try_from(u32::from(PortName::NULL)).unwrap();
    panic!("mach_msg_trap: putting the reply, not yet")
}
