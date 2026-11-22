// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};

use crate::arch::PAGE_SIZE;
use crate::ipc::MessageType;
use crate::ipc::entry::Rights;
use crate::ipc::kernel_message::header::{Header, MessageBits};
use crate::ipc::kernel_message::{self, EXPAND_FACTOR, KernelMessage, SAVED_MESSAGE_SIZE};
use crate::ipc::object::Object;
use crate::ipc::port::port::{Port, Receiver};
use crate::ipc::port::port_name::PortName;
use crate::ipc::space::KERNEL_SPACE;
use crate::reference::Reference;
use crate::{ipc_kobject, smp};

use super::queue::queue;
use super::receive::receive;
use super::send::{send, send_message, send_right};

bitflags::bitflags! {
    /// What a message trap does, as Mach's option bits; only those used
    /// here.
    #[derive(Clone, Copy)]
    pub(super) struct Options: u32 {
        /// Send a message.
        const SEND = 1;
        /// Receive a message.
        const RECEIVE = 1 << 1;
        /// Give up receiving after the timeout.
        const RECEIVE_TIMEOUT = 1 << 8;
        /// Ask for a dead-name notification on the received reply right.
        const RECEIVE_NOTIFY = 1 << 9;
        /// Send past the queue's limit; the kernel's alone.
        const SEND_ALWAYS = 1 << 16;
    }
}

/// Sends the message at `message`, `send_size` bytes, then receives one on
/// `receive_name`, as `mach_msg_trap` does; so far a request to a port the
/// kernel receives on, and its reply, a request to a name with no entry,
/// which fails, a message to the sender's own receive right or a reply,
/// sent then received, or a send or a receive alone. Gives
/// `MACH_MSG_SUCCESS`, or the send's or the receive's status.
///
/// # Panics
///
/// If no thread runs on the CPU, or the reply port has no send-once right.
/// On every other path: not yet; as `kernel_message::get`,
/// `KernelMessage::copy_in_header`, `KernelMessage::copy_in_body`,
/// `ipc_kobject::server`, `KernelMessage::copy_out_body`,
/// `kernel_message::put`, `send`, `send_message`, `queue` and `receive`.
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
    timeout: u32,
    _notify: PortName,
) -> u32 {
    if option != (Options::SEND | Options::RECEIVE).bits() {
        // Of mach_msg_send and mach_msg_receive, so far either alone, the
        // send with no other option.
        if option == Options::SEND.bits() {
            return send(message, send_size);
        }
        let options = Options::from_bits_retain(option);
        assert!(
            options.contains(Options::RECEIVE) && !options.contains(Options::SEND),
            "mach_msg_trap: option {option:#x}, not yet"
        );
        return receive(message, options, receive_size, receive_name, timeout);
    }
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
    let mut request = fits
        .then(KernelMessage::take_cached)
        .flatten()
        .and_then(|mut cached| cached.copy_in(message, send_size).is_ok().then_some(cached))
        .unwrap_or_else(|| kernel_message::get(message, send_size));
    // slow_copyin: the general copy-in, then slow_send and slow_get_rcv_port
    // (no handoff); a failed copy-in frees the message.
    let slow_copy_in = |mut request: KernelMessage| {
        if let Err(status) = request.copy_in_header(space) {
            // ikm_free.
            drop(request);
            return status;
        }
        if request.header().bits.is_complex() {
            request.copy_in_body(space, &thread.task().map);
        }
        // SAFETY: copied in, the destination is a port's exposed address, and
        // the message holds a reference to it; this borrow ends at the unlock.
        let destination =
            unsafe { &*ptr::with_exposed_provenance::<Port>(request.header().remote_port) };
        assert!(
            destination.state.lock().receiver != Receiver::Space(NonNull::from(&KERNEL_SPACE)),
            "mach_msg_trap: kernel_send from slow_copyin, not yet"
        );
        queue(request, Options::empty());
        receive(
            message,
            Options::RECEIVE,
            receive_size,
            receive_name,
            timeout,
        )
    };
    // fast_copyin.
    let header = request.header_mut();
    // A reply, a moved send-once right and no reply port: sent, then the
    // receive, as the general path does (no handoff).
    let reply = MessageBits::new(Some(MessageType::PortSendOnce), None);
    if header.bits == reply || header.bits == reply.with_complex() {
        let status = send_message(request);
        if status != 0 {
            return status;
        }
        return receive(
            message,
            Options::RECEIVE,
            receive_size,
            receive_name,
            timeout,
        );
    }
    // A copied send right for the destination, and a send-once right made
    // for the reply port; a complex message's rights go through
    // slow_copyin's ipc_kmsg_copyin_header the same way.
    let complex = header.bits.is_complex();
    let bits = MessageBits::new(Some(MessageType::CopySend), Some(MessageType::MakeSendOnce));
    if header.bits != if complex { bits.with_complex() } else { bits } {
        return slow_copy_in(request);
    }
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
    let Some(destination_port) = send_right(
        &state,
        PortName::from(u32::try_from(header.remote_port).unwrap()),
    ) else {
        // abort_request_copyin.
        drop(state);
        return slow_copy_in(request);
    };
    let Some(Object::Port(reply_port)) = &reply.object else {
        panic!("mach_msg_trap: an entry with no port");
    };
    // The message's references, taken while the space's lock keeps the
    // entries' own.
    let (reply_port, destination_port) = (reply_port.clone(), destination_port.clone());
    let mut destination_state = destination_port.state.lock();
    let Some(mut reply_state) = destination_state
        .active
        .then(|| reply_port.state.try_lock())
        .flatten()
    else {
        // abort_request_copyin; the references go after the locks.
        drop(destination_state);
        drop(state);
        drop(destination_port);
        drop(reply_port);
        return slow_copy_in(request);
    };
    drop(state);
    destination_state.send_rights += 1;
    reply_state.send_once_rights += 1;
    let bits = MessageBits::new(Some(MessageType::PortSend), Some(MessageType::PortSendOnce));
    header.bits = if complex { bits.with_complex() } else { bits };
    let kernel = destination_state.receiver == Receiver::Space(NonNull::from(&KERNEL_SPACE));
    drop(reply_state);
    drop(destination_state);
    // The header's slots take the message's references.
    header.remote_port = Reference::into_raw(destination_port)
        .as_ptr()
        .expose_provenance();
    header.local_port = Reference::into_raw(reply_port).as_ptr().expose_provenance();
    if complex {
        // slow_copyin: its body's rights, then back to kernel_send.
        request.copy_in_body(space, &thread.task().map);
    }
    assert!(kernel, "mach_msg_trap: fast_send_receive, not yet");
    // kernel_send.
    let Some(mut reply) = ipc_kobject::server(request) else {
        panic!("mach_msg_trap: slow_get_rcv_port, not yet");
    };
    // SAFETY: the reply's destination is the exposed address of a reference
    // to the reply port given up by `into_raw`, which the handle takes, as
    // the slot is overwritten below.
    let reply_port = unsafe {
        Reference::from_raw(
            NonNull::new(ptr::with_exposed_provenance_mut::<Port>(
                reply.header().remote_port,
            ))
            .unwrap(),
        )
    };
    let mut reply_state = reply_port.state.lock();
    // Received directly only from a live port this space receives on under
    // `receive_name`, in no set, with no thread waiting and nothing queued.
    assert!(
        reply_state.active
            && reply_state.receiver == Receiver::Space(NonNull::from(&**space))
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
    // A reply, with the send-once right it carries; complex if its body
    // carries rights or out-of-line memory.
    let complex = reply.header().bits.is_complex();
    let bits = MessageBits::new(Some(MessageType::PortSendOnce), None);
    assert!(
        reply.header().bits == if complex { bits.with_complex() } else { bits },
        "mach_msg_trap: fast_copyout, bits {:#x}, not yet",
        u32::from(reply.header().bits)
    );
    let mut reply_state = reply_port.state.lock();
    assert!(
        reply_state.active,
        "mach_msg_trap: slow_copyout, a dead port, not yet"
    );
    let payload = reply_state.protected_payload;
    assert!(
        reply_state.receiver == Receiver::Space(NonNull::from(&**space)),
        "mach_msg_trap: ipc_notify_send_once, not yet"
    );
    reply_state.send_once_rights = reply_state
        .send_once_rights
        .checked_sub(1)
        .expect("mach_msg_trap: no send-once right");
    let name = reply_state.receiver_name;
    drop(reply_state);
    // This space receives on it, so the right goes with its reference.
    drop(reply_port);
    assert!(
        payload.is_none(),
        "mach_msg_trap: a protected payload, not yet"
    );
    let header = reply.header_mut();
    let bits = MessageBits::new(None, Some(MessageType::PortSendOnce));
    header.bits = if complex { bits.with_complex() } else { bits };
    header.local_port = usize::try_from(u32::from(name)).unwrap();
    header.remote_port = usize::try_from(u32::from(PortName::NULL)).unwrap();
    if complex {
        reply.copy_out_body(space, &thread.task().map);
    }
    // fast_put: only a saved buffer is copied out here.
    assert!(
        reply.size() != PAGE_SIZE,
        "mach_msg_trap: fast_put of a saved buffer, not yet"
    );
    // slow_put.
    kernel_message::put(message, reply, reply_size);
    // MACH_MSG_SUCCESS.
    0
}
