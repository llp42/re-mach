// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};

use crate::ipc::MessageType;
use crate::ipc::entry::Rights;
use crate::ipc::kernel_message::header::MessageBits;
use crate::ipc::kernel_message::{self, KernelMessage};
use crate::ipc::object::Object;
use crate::ipc::port::port::{Port, Receiver};
use crate::ipc::port::port_name::PortName;
use crate::ipc::space::{KERNEL_SPACE, SpaceState};
use crate::reference::Reference;
use crate::{ipc_kobject, smp};

use super::queue::queue;
use super::trap::Options;

/// Sends the message at `message`, `send_size` bytes, as `mach_msg_send`
/// does. Gives its status.
///
/// # Panics
///
/// As `kernel_message::get` and `send_message`.
pub(super) fn send(message: usize, send_size: u32) -> u32 {
    // ipc_kmsg_get.
    send_message(kernel_message::get(message, send_size))
}

/// Sends `request`, copied from the current thread's user memory, as
/// `mach_msg_send` does once it has the message: its header's rights and
/// its body's are copied in, then it is served if the kernel receives on
/// its destination, else queued. Gives `MACH_MSG_SUCCESS`, or the copy-in's
/// status, which frees it.
///
/// # Panics
///
/// If no thread runs on the CPU, or the kernel's port is dead. On every
/// other path: not yet; as `KernelMessage::copy_in_header`,
/// `KernelMessage::copy_in_body`, `ipc_kobject::server` and `queue`.
pub(super) fn send_message(mut request: KernelMessage) -> u32 {
    let thread = smp::current()
        .active_thread()
        .expect("mach_msg_send: no current thread");
    let space = &thread.task().space;
    // ipc_kmsg_copyin_header's common cases: an asynchronous message, a
    // copied send right for the destination, or a reply, a moved send-once
    // right, and no reply port; else its general case.
    let header = request.header_mut();
    let complex = header.bits.is_complex();
    let with_complex = |bits: MessageBits| if complex { bits.with_complex() } else { bits };
    let copied = with_complex(MessageBits::new(Some(MessageType::CopySend), None));
    let reply = with_complex(MessageBits::new(Some(MessageType::PortSendOnce), None));
    if header.bits == copied || header.bits == reply {
        assert!(
            header.local_port == 0,
            "ipc_kmsg_copyin_header: MACH_SEND_INVALID_HEADER, not yet"
        );
        let name = PortName::from(u32::try_from(header.remote_port).unwrap());
        let port = if header.bits == copied {
            let state = space.state.read();
            assert!(state.active, "ipc_kmsg_copyin_header: abort_async, not yet");
            // The message's reference, taken while the space's lock keeps the
            // entry's own.
            let port = send_right(&state, name)
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
            header.bits = with_complex(MessageBits::new(Some(MessageType::PortSend), None));
            port
        } else {
            // The reply case: the entry, exactly a send-once right, is freed, and
            // the message takes its right and its reference.
            let mut state = space.state.write();
            assert!(state.active, "ipc_kmsg_copyin_header: abort_reply, not yet");
            let entry = state
                .lookup(name)
                .filter(|entry| entry.bits.rights.bits() == Rights::SEND_ONCE.bits())
                .unwrap_or_else(|| panic!("ipc_kmsg_copyin_header: abort_reply, not yet"));
            assert!(
                entry.bits.user_references == 1 && !entry.bits.message_accepted,
                "ipc_kmsg_copyin_header: a send-once right's user references or message-accepted request"
            );
            assert!(
                entry.request == 0,
                "ipc_kmsg_copyin_header: a dead-name request, abort_reply, not yet"
            );
            let Some(Object::Port(port)) = &entry.object else {
                panic!("ipc_kmsg_copyin_header: a send-once right with no port");
            };
            let port_state = port.state.lock();
            assert!(
                port_state.active,
                "ipc_kmsg_copyin_header: a dead destination, abort_reply, not yet"
            );
            assert!(
                port_state.send_once_rights > 0,
                "ipc_kmsg_copyin_header: a port with no send-once right"
            );
            drop(port_state);
            let entry = state
                .map
                .get_mut(name.key())
                .expect("ipc_kmsg_copyin_header: no entry");
            let Some(Object::Port(port)) = entry.object.take() else {
                panic!("ipc_kmsg_copyin_header: a send-once right with no port");
            };
            state.deallocate(name);
            drop(state);
            header.bits = with_complex(MessageBits::new(Some(MessageType::PortSendOnce), None));
            port
        };
        // The header's slot takes the message's reference.
        header.remote_port = Reference::into_raw(port).as_ptr().expose_provenance();
    } else if let Err(status) = request.copy_in_header(space) {
        // ikm_free.
        drop(request);
        return status;
    }
    if request.header().bits.is_complex() {
        request.copy_in_body(space, &thread.task().map);
    }
    // ipc_mqueue_send: served here if the kernel receives on the port.
    // SAFETY: copied in, the destination is a port's exposed address, and
    // the message holds a reference to it, which the server releases or the
    // queue takes only once this borrow has ended.
    let port_state =
        unsafe { &*ptr::with_exposed_provenance::<Port>(request.header().remote_port) }
            .state
            .lock();
    if port_state.receiver == Receiver::Space(NonNull::from(&KERNEL_SPACE)) {
        // A kernel port's receiver is cleared before it dies.
        assert!(port_state.active, "ipc_mqueue_send: a dead kernel port");
        drop(port_state);
        let reply = ipc_kobject::server(request);
        assert!(
            reply.is_none(),
            "ipc_mqueue_send: ipc_mqueue_send_always, not yet"
        );
    } else {
        drop(port_state);
        // As mach_msg_send's options: MACH_SEND_TIMEOUT, not yet.
        queue(request, Options::empty());
    }
    // MACH_MSG_SUCCESS.
    0
}

/// Sends `message`, as `ipc_mqueue_send_always` does: past its
/// destination's queue limit.
///
/// # Panics
///
/// If the kernel receives on its destination: not yet. As `queue`.
pub fn send_always(message: KernelMessage) {
    // SAFETY: copied in or made by the kernel, the destination is a port's
    // exposed address, and the message holds a reference to it.
    let port = unsafe { &*ptr::with_exposed_provenance::<Port>(message.header().remote_port) };
    let port_state = port.state.lock();
    assert!(
        port_state.receiver != Receiver::Space(NonNull::from(&KERNEL_SPACE)),
        "ipc_mqueue_send: ipc_kobject_server, not yet"
    );
    drop(port_state);
    queue(message, Options::SEND_ALWAYS);
}

/// The port of the send right alone that `name` denotes in `state`, as the
/// fast copy-ins look a destination up; `None` for any other entry.
///
/// # Panics
///
/// If its entry has no port.
pub(super) fn send_right(state: &SpaceState, name: PortName) -> Option<&Reference<Port>> {
    let entry = state
        .lookup(name)
        .filter(|entry| entry.bits.rights.bits() == Rights::SEND.bits())?;
    let Some(Object::Port(port)) = &entry.object else {
        panic!("ipc_right_copyin: a send right with no port");
    };
    Some(port)
}
