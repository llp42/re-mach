// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};
use core::sync::atomic::Ordering;

use crate::ipc::MessageType;
use crate::ipc::kernel_message::KernelMessage;
use crate::ipc::port::port::Port;
use crate::ipc::port::state::PortState;
use crate::reference::Reference;
use crate::sched::{self, wait::WaitResult};
use crate::smp;
use crate::sync::SpinlockGuard;

use super::receive::{MACH_RCV_INTERRUPTED, MACH_RCV_TIMED_OUT};
use super::trap::Options;

/// Queues `message` on its destination, a port the kernel does not receive
/// on, as `ipc_mqueue_send` does past the kernel's branch; `SEND_ALWAYS` in
/// `options` passes the queue's limit. The port's oldest waiting receiver
/// wakes once the port is unlocked, and takes the message itself; an
/// orphaned reply port with every send-once right now in its queue is then
/// destroyed (SC-D2).
///
/// # Panics
///
/// If no thread runs on the CPU, a waiting receiver is being freed, or an
/// orphaned reply port has send rights. If the port is dead or in a set,
/// or a send right finds its queue full: not yet. As
/// `Port::deallocate_reply`.
pub(super) fn queue(message: KernelMessage, options: Options) {
    let thread = smp::current()
        .active_thread()
        .expect("ipc_mqueue_send: no current thread");
    // SAFETY: copied in, the destination is a port's exposed address, and
    // the message holds a reference to it, which it keeps queued; this
    // borrow ends at the unlock.
    let port = unsafe { &*ptr::with_exposed_provenance::<Port>(message.header().remote_port) };
    let mut port_state = port.state.lock();
    assert!(port_state.active, "ipc_mqueue_send: a dead port, not yet");
    // Under the limit, MACH_SEND_ALWAYS, or a send-once right: no wait.
    assert!(
        port_state.message_count < port_state.queue_limit
            || options.contains(Options::SEND_ALWAYS)
            || matches!(
                message.header().bits.remote(),
                Some(MessageType::PortSendOnce)
            ),
        "ipc_mqueue_send: a full queue, ip_blocked, not yet"
    );
    port_state.message_count += 1;
    assert!(
        port_state.set.is_none(),
        "ipc_mqueue_send: a port in a set, not yet"
    );
    message.enqueue(&mut port_state.messages);
    // The oldest, with a reference that keeps it through the wake.
    let receiver = port_state.receivers.pop_back().map(|receiver| {
        // SAFETY: a thread lives while it waits on a port's queue.
        unsafe { Reference::upgrade(NonNull::from(receiver)) }
            .expect("ipc_mqueue_send: a receiver being freed")
    });
    // An orphaned reply port with every send-once right in its queue has no
    // receiver to come: destroyed below, with a reference of its own.
    let orphan = (port_state.orphaned && port_state.send_once_rights_queued()).then(|| {
        assert!(
            port_state.send_rights == 0,
            "ipc_mqueue_send: an orphaned reply port with send rights"
        );
        // Only its ended thread received on it.
        assert!(
            receiver.is_none(),
            "ipc_mqueue_send: a receiver on an orphaned reply port"
        );
        // SAFETY: the message just queued holds a reference to it.
        unsafe { Reference::upgrade(NonNull::from(port)) }
            .expect("ipc_mqueue_send: a port being freed")
    });
    drop(port_state);
    if let Some(receiver) = receiver {
        // SAFETY: the reference keeps it alive through the wake, and a
        // runnable thread holds its own.
        sched::wait::clear_wait(
            unsafe { Reference::as_ptr(&receiver).as_ref() },
            WaitResult::Awakened,
        );
    }
    if let Some(orphan) = orphan {
        orphan.deallocate_reply();
        drop(orphan);
    }
    thread.task().messages_sent.fetch_add(1, Ordering::Relaxed);
}

/// Takes the first message off the queue of `port`, which `port_state`
/// holds locked and alive, waiting for one at most `timeout` milliseconds
/// if given, as `ipc_mqueue_receive` does: gives it with its sequence
/// number, taken now, else `MACH_RCV_TIMED_OUT`. The port is unlocked. A
/// receiver a sender woke, off the port's receivers, looks at the queue
/// again.
///
/// # Panics
///
/// If no thread runs on the CPU, or it wakes for no cause. If the port
/// dies while it waits: not yet.
pub(super) fn dequeue<'a>(
    port: &'a Port,
    mut port_state: SpinlockGuard<'a, PortState>,
    mut timeout: Option<u32>,
) -> Result<(KernelMessage, u32), u32> {
    let thread = smp::current()
        .active_thread()
        .expect("ipc_mqueue_receive: no current thread");
    // Under the queue's lock, the port's.
    loop {
        if let Some(message) = KernelMessage::dequeue(&mut port_state.messages) {
            let sequence_number = port_state.sequence_number;
            port_state.sequence_number = sequence_number.wrapping_add(1);
            // No sender waits on a full queue yet.
            port_state.message_count -= 1;
            drop(port_state);
            thread
                .task()
                .messages_received
                .fetch_add(1, Ordering::Relaxed);
            return Ok((message, sequence_number));
        }
        match timeout {
            Some(0) => {
                drop(port_state);
                return Err(MACH_RCV_TIMED_OUT);
            }
            Some(milliseconds) => sched::wait::will_wait_with_timeout(milliseconds),
            None => sched::wait::will_wait(),
        }
        port_state.receivers.push_front(thread);
        drop(port_state);
        sched::switch::block();
        port_state = port.state.lock();
        assert!(
            port_state.active,
            "ipc_mqueue_receive: MACH_RCV_PORT_DIED, not yet"
        );
        // A sender took it off the queue, and queued a message; if another
        // receiver took that, it waits again, at once if its timer fired.
        if !thread.is_queued_to_receive() {
            if matches!(thread.wait_result(), WaitResult::TimedOut) {
                timeout = Some(0);
            }
            // Else the time left is not tracked: a full wait (SC-D3, several
            // receivers only).
            continue;
        }
        // Still queued, it woke for no message: MACH_RCV_IN_PROGRESS.
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
            WaitResult::Interrupted => {
                drop(port_state);
                return Err(MACH_RCV_INTERRUPTED);
            }
            WaitResult::Awakened => panic!("ipc_mqueue_receive: awakened with no message"),
        }
    }
}
