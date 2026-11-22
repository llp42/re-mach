// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;

use intrusive_collections::LinkedList;

use super::kernel_object::KernelObject;
use super::port::Receiver;
use super::port_name::PortName;
use crate::ipc::MessageType;
use crate::ipc::kernel_message;
use crate::ipc::port_set::PortSet;
use crate::thread::thread::IpcQueueAdapter;
/// A port's state under its lock; so far only whether it is alive, its
/// receiver and its name there, its send and send-once rights, what it
/// stands for, its set, its sequence number, its message queue, its count
/// and limit, its protected payload, and whether it is an orphaned reply
/// port.
pub(in crate::ipc) struct PortState {
    /// Whether it is alive.
    pub(in crate::ipc) active: bool,
    /// Who holds its receive right.
    pub(in crate::ipc) receiver: Receiver,
    /// Its receive right's name in that space.
    pub(in crate::ipc) receiver_name: PortName,
    /// How many send rights for it there are.
    pub(in crate::ipc) send_rights: u32,
    /// How many send-once rights for it there are.
    pub(in crate::ipc) send_once_rights: u32,
    /// How many send rights have been made from its receive right.
    pub(in crate::ipc) make_send_count: u32,
    /// What it stands for, when the kernel receives on it.
    pub(super) kernel_object: Option<KernelObject>,
    /// The set it is in, if any.
    pub(in crate::ipc) set: Option<NonNull<PortSet>>,
    /// The sequence number of the next message received from it.
    pub(in crate::ipc) sequence_number: u32,
    /// The messages queued on it, oldest first.
    pub(in crate::ipc) messages: LinkedList<kernel_message::QueueAdapter>,
    /// How many messages are queued on it.
    pub(in crate::ipc) message_count: u32,
    /// How many messages it queues before senders wait.
    pub(in crate::ipc) queue_limit: u32,
    /// The threads waiting to receive from it, latest first.
    pub(in crate::ipc) receivers: LinkedList<IpcQueueAdapter>,
    /// What its received messages carry in place of its name, if set.
    pub(in crate::ipc) protected_payload: Option<u64>,
    /// Whether, as a reply port, its thread ended: it is destroyed once every
    /// send-once right for it sits in its own queue.
    pub(in crate::ipc) orphaned: bool,
}

// SAFETY: its pointers are to spaces, sets, kernel objects, queued messages
// and waiting threads, which any CPU may use; its lock serializes changes
// to them.
unsafe impl Send for PortState {}

impl PortState {
    /// Whether every send-once right for it sits in its own queue, one per
    /// queued message whose destination is a send-once right; a right in
    /// flight counts as out, so this errs toward false. Under its lock.
    pub(in crate::ipc) fn send_once_rights_queued(&self) -> bool {
        let queued = self
            .messages
            .iter()
            .filter(|buffer| {
                matches!(
                    buffer.header().bits.remote(),
                    Some(MessageType::PortSendOnce)
                )
            })
            .count();
        usize::try_from(self.send_once_rights).unwrap() == queued
    }

    /// Readies its port's receive right to be destroyed, its port alive, as
    /// `ipc_port_clear_receiver` does.
    ///
    /// # Panics
    ///
    /// If its port is dead. If it is in a set, or a thread waits to receive
    /// from it: not yet.
    pub(in crate::ipc) fn clear_receiver(&mut self) {
        assert!(self.active, "ipc_port_clear_receiver: a dead port");
        assert!(
            self.set.is_none(),
            "ipc_port_clear_receiver: ipc_pset_remove, not yet"
        );
        // As ipc_mqueue_changed, under the queue's lock, the port's.
        assert!(
            self.receivers.is_empty(),
            "ipc_mqueue_changed: thread_go, not yet"
        );
        // As ipc_port_set_mscount.
        self.make_send_count = 0;
        self.sequence_number = 0;
    }
}
