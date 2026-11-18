// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The message trap.

use core::ptr::{self, NonNull};

use intrusive_collections::UnsafeRef;

use super::MessageType;
use super::entry::Rights;
use super::kernel_message::{
    self, EXPAND_FACTOR, Header, KernelMessage, MessageBits, SAVED_MESSAGE_SIZE,
};
use super::object::Object;
use super::port::{Port, PortName, Receiver};
use super::space::{KERNEL_SPACE, SpaceState};
use crate::arch::PAGE_SIZE;
use crate::reference::Reference;
use crate::sched::{self, WaitResult};
use crate::{ipc_kobject, smp};

bitflags::bitflags! {
    /// What a message trap does, as Mach's option bits; only those used
    /// here.
    struct Options: u32 {
        /// Send a message.
        const SEND = 1;
        /// Receive a message.
        const RECEIVE = 1 << 1;
        /// Give up receiving after the timeout.
        const RECEIVE_TIMEOUT = 1 << 8;
    }
}

/// A receive's status when no message came in time, as Mach numbers it.
const MACH_RCV_TIMED_OUT: u32 = 0x1000_4003;

/// Sends the message at `message`, `send_size` bytes, then receives one on
/// `receive_name`, as `mach_msg_trap` does; so far a request to a port the
/// kernel receives on, and its reply, or a send or a receive alone. Gives
/// `MACH_MSG_SUCCESS`, or the receive's status.
///
/// # Panics
///
/// If no thread runs on the CPU, or the reply port has no send-once right.
/// On every other path: not yet; as `kernel_message::get`,
/// `KernelMessage::copy_in_body`, `ipc_kobject::server`,
/// `KernelMessage::copy_out_body`, `kernel_message::put`, `send` and
/// `receive`.
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
        return receive(
            receive_name,
            options
                .contains(Options::RECEIVE_TIMEOUT)
                .then_some(timeout),
        );
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
    // fast_copyin.
    let header = request.header_mut();
    // A copied send right for the destination, and a send-once right made
    // for the reply port; a complex message's rights go through
    // slow_copyin's ipc_kmsg_copyin_header the same way.
    let complex = header.bits.is_complex();
    let bits = MessageBits::new(Some(MessageType::CopySend), Some(MessageType::MakeSendOnce));
    assert!(
        header.bits == if complex { bits.with_complex() } else { bits },
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
    let destination_port = send_right(
        &state,
        PortName::from(u32::try_from(header.remote_port).unwrap()),
    )
    .unwrap_or_else(|| panic!("mach_msg_trap: slow_copyin, not yet"));
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
        panic!("mach_msg_trap: slow_copyin, not yet");
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

/// Sends the message at `message`, `send_size` bytes, as `mach_msg_send`
/// does; so far a simple message with no reply port to a port the kernel
/// receives on. Gives `MACH_MSG_SUCCESS`.
///
/// # Panics
///
/// If no thread runs on the CPU, or the kernel's port is dead. On every
/// other path: not yet; as `kernel_message::get` and `ipc_kobject::server`.
fn send(message: usize, send_size: u32) -> u32 {
    let thread = smp::current()
        .active_thread()
        .expect("mach_msg_send: no current thread");
    let space = &thread.task().space;
    // ipc_kmsg_get.
    let mut request = kernel_message::get(message, send_size);
    // ipc_kmsg_copyin_header: so far an asynchronous message, a copied send
    // right for the destination and no reply port.
    let header = request.header_mut();
    let complex = header.bits.is_complex();
    let bits = MessageBits::new(Some(MessageType::CopySend), None);
    assert!(
        header.bits == if complex { bits.with_complex() } else { bits },
        "ipc_kmsg_copyin_header: header bits {:#x}, not yet",
        u32::from(header.bits)
    );
    assert!(
        header.local_port == 0,
        "ipc_kmsg_copyin_header: MACH_SEND_INVALID_HEADER, not yet"
    );
    let state = space.state.read();
    assert!(state.active, "ipc_kmsg_copyin_header: abort_async, not yet");
    // The message's reference, taken while the space's lock keeps the
    // entry's own.
    let port = send_right(
        &state,
        PortName::from(u32::try_from(header.remote_port).unwrap()),
    )
    .unwrap_or_else(|| panic!("ipc_kmsg_copyin_header: abort_async, not yet"))
    .clone();
    let mut port_state = port.state.lock();
    drop(state);
    assert!(
        port_state.active,
        "ipc_kmsg_copyin_header: a dead destination, not yet"
    );
    port_state.send_rights += 1;
    drop(port_state);
    let bits = MessageBits::new(Some(MessageType::PortSend), None);
    header.bits = if complex { bits.with_complex() } else { bits };
    // The header's slot takes the message's reference.
    let destination = Reference::into_raw(port);
    header.remote_port = destination.as_ptr().expose_provenance();
    assert!(!complex, "ipc_kmsg_copyin: ipc_kmsg_copyin_body, not yet");
    // ipc_mqueue_send: served here if the kernel receives on the port.
    // SAFETY: from `into_raw`, the port lives while the message's reference
    // does, which the server releases only once this borrow has ended.
    let port_state = unsafe { destination.as_ref() }.state.lock();
    assert!(
        port_state.receiver == Receiver::Space(NonNull::from(&KERNEL_SPACE)),
        "ipc_mqueue_send: a port the kernel does not receive on, not yet"
    );
    // A kernel port's receiver is cleared before it dies.
    assert!(port_state.active, "ipc_mqueue_send: a dead kernel port");
    drop(port_state);
    let reply = ipc_kobject::server(request);
    assert!(
        reply.is_none(),
        "ipc_mqueue_send: ipc_mqueue_send_always, not yet"
    );
    // MACH_MSG_SUCCESS.
    0
}

/// Receives a message on `name`, as `mach_msg_receive` does, waiting for one
/// at most `timeout` milliseconds if given; so far none comes. Gives
/// `MACH_RCV_TIMED_OUT`.
///
/// # Panics
///
/// If no thread runs on the CPU, or it wakes for no cause. On every other
/// path: not yet.
fn receive(name: PortName, mut timeout: Option<u32>) -> u32 {
    let thread = smp::current()
        .active_thread()
        .expect("mach_msg_receive: no current thread");
    let space = &thread.task().space;
    // ipc_mqueue_copyin: the port locked, and a reference to it taken while
    // the space's lock keeps the entry's own.
    let state = space.state.read();
    assert!(
        state.active,
        "ipc_mqueue_copyin: MACH_RCV_INVALID_NAME, not yet"
    );
    let entry = state
        .lookup(name)
        .unwrap_or_else(|| panic!("ipc_mqueue_copyin: MACH_RCV_INVALID_NAME, not yet"));
    let port = if entry.bits.rights.contains(Rights::RECEIVE) {
        let Some(Object::Port(port)) = &entry.object else {
            panic!("ipc_mqueue_copyin: an entry with no port");
        };
        port.clone()
    } else if entry.bits.rights.contains(Rights::PORT_SET) {
        panic!("ipc_mqueue_copyin: a port set, not yet");
    } else {
        panic!("ipc_mqueue_copyin: MACH_RCV_INVALID_NAME, not yet");
    };
    let mut port_state = port.state.lock();
    assert!(
        port_state.active
            && port_state.receiver_name == name
            && port_state.receiver == Receiver::Space(NonNull::from(&**space)),
        "ipc_mqueue_copyin: a receive right not the space's"
    );
    drop(state);
    assert!(
        port_state.set.is_none(),
        "ipc_mqueue_copyin: a port in a set, not yet"
    );
    // ipc_mqueue_receive, under the queue's lock, the port's.
    let status = loop {
        assert!(
            port_state.messages.is_empty(),
            "ipc_mqueue_receive: a queued message, not yet"
        );
        match timeout {
            Some(0) => break MACH_RCV_TIMED_OUT,
            Some(milliseconds) => sched::will_wait_with_timeout(milliseconds),
            None => sched::will_wait(),
        }
        // SAFETY: a thread lives while it waits on a port's queue, and it
        // leaves it below.
        port_state
            .receivers
            .push_front(unsafe { UnsafeRef::from_raw(thread) });
        drop(port_state);
        sched::block();
        port_state = port.state.lock();
        // Still queued, it woke for no message: MACH_RCV_IN_PROGRESS.
        assert!(
            thread.is_queued_to_receive(),
            "ipc_mqueue_receive: ith_state not MACH_RCV_IN_PROGRESS, not yet"
        );
        // SAFETY: still linked, it is on the one queue it joined, this one.
        unsafe { port_state.receivers.cursor_mut_from_ptr(thread) }.remove();
        match thread.wait_result() {
            WaitResult::TimedOut => {
                assert!(
                    timeout.is_some(),
                    "ipc_mqueue_receive: timed out with no timeout"
                );
                timeout = Some(0);
            }
            WaitResult::Awakened => panic!("ipc_mqueue_receive: awakened with no message"),
        }
    };
    drop(port_state);
    // ipc_object_release.
    drop(port);
    status
}

/// The port of the send right alone that `name` denotes in `state`, as the
/// fast copy-ins look a destination up; `None` for any other entry.
///
/// # Panics
///
/// If its entry has no port.
fn send_right(state: &SpaceState, name: PortName) -> Option<&Reference<Port>> {
    let entry = state
        .lookup(name)
        .filter(|entry| entry.bits.rights.bits() == Rights::SEND.bits())?;
    let Some(Object::Port(port)) = &entry.object else {
        panic!("ipc_right_copyin: a send right with no port");
    };
    Some(port)
}
