// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;

use crate::ipc::entry::Rights;
use crate::ipc::kernel_message::{self, KernelMessage};
use crate::ipc::object::Object;
use crate::ipc::port::port::{Port, Receiver};
use crate::ipc::port::port_name::PortName;
use crate::smp;

use super::queue::dequeue;
use super::trap::Options;

/// A receive's status when no message came in time, as Mach numbers it.
pub(super) const MACH_RCV_TIMED_OUT: u32 = 0x1000_4003;

/// A receive's status when its wait was interrupted, as Mach numbers it.
pub const MACH_RCV_INTERRUPTED: u32 = 0x1000_4005;

/// Receives a message on `name` into the user's buffer at `message`, at
/// most `receive_size` bytes, as `mach_msg_receive` does with `options`,
/// waiting for one at most `timeout` milliseconds with `RECEIVE_TIMEOUT`:
/// its header and body are copied out to the receiver. Gives
/// `MACH_MSG_SUCCESS`, or `MACH_RCV_TIMED_OUT`.
///
/// # Panics
///
/// If no thread runs on the CPU. If the message is larger than
/// `receive_size`, or with `RECEIVE_NOTIFY`: not yet. On every other path:
/// not yet; as `dequeue`, `KernelMessage::copy_out_header`,
/// `KernelMessage::copy_out_body` and `kernel_message::put`.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the port's guard is moved into dequeue"
)]
pub(super) fn receive(
    message: usize,
    options: Options,
    receive_size: u32,
    name: PortName,
    timeout: u32,
) -> u32 {
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
    let port_state = port.state.lock();
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
    let result = dequeue(
        &port,
        port_state,
        options
            .contains(Options::RECEIVE_TIMEOUT)
            .then_some(timeout),
    );
    // ipc_object_release.
    drop(port);
    let (mut received, sequence_number) = match result {
        Ok(received) => received,
        Err(status) => return status,
    };
    received.header_mut().sequence_number = sequence_number;
    let size = received.header().size;
    assert!(
        size <= receive_size,
        "mach_msg_receive: MACH_RCV_TOO_LARGE, not yet"
    );
    assert!(
        !options.contains(Options::RECEIVE_NOTIFY),
        "mach_msg_receive: MACH_RCV_NOTIFY, not yet"
    );
    // ipc_kmsg_copyout, with no notify port.
    received.copy_out_header(space);
    if received.header().bits.is_complex() {
        received.copy_out_body(space, &thread.task().map);
    }
    kernel_message::put(message, received, size);
    // MACH_MSG_SUCCESS.
    0
}

/// Waits on `port`, a reply port the caller holds a reference to, for its
/// reply, as `exception_raise` receives it: with no option, no size limit
/// and no timeout; gives it with its sequence number, or the receive's
/// status.
///
/// # Panics
///
/// If `port` is dead: not yet. As `dequeue`.
pub fn receive_reply(port: &Port) -> Result<(KernelMessage, u32), u32> {
    let port_state = port.state.lock();
    assert!(
        port_state.active,
        "exception_raise: MACH_RCV_PORT_DIED, not yet"
    );
    dequeue(port, port_state, None)
}
