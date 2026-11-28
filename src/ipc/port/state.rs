// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;

use tailq::TailQueue;

use super::kernel_object::KernelObject;
use super::port::{Port, Receiver};
use super::port_name::PortName;
use crate::ipc::MessageType;
use crate::ipc::kernel_message;
use crate::ipc::port_set::PortSet;
use crate::reference::Reference;
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
    pub(in crate::ipc) messages: TailQueue<'static, kernel_message::QueueAdapter>,
    /// How many messages are queued on it.
    pub(in crate::ipc) message_count: u32,
    /// How many messages it queues before senders wait.
    pub(in crate::ipc) queue_limit: u32,
    /// The threads waiting to receive from it, latest first.
    pub(in crate::ipc) receivers: TailQueue<'static, IpcQueueAdapter>,
    /// What its received messages carry in place of its name, if set.
    pub(in crate::ipc) protected_payload: Option<u64>,
    /// Whether, as a reply port, its thread ended: it is destroyed once every
    /// send-once right for it sits in its own queue.
    pub(in crate::ipc) orphaned: bool,
    /// The send-once right for a no-senders notification, if requested.
    pub(in crate::ipc) no_senders: Option<Reference<Port>>,
    /// The send-once right for a port-destroyed notification, if requested.
    pub(in crate::ipc) port_destroyed: Option<Reference<Port>>,
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

    /// Takes its no-senders request, if it has no send rights, as
    /// `ipc_port_release_send` does; gives the right and the make-send
    /// count it reports.
    pub(in crate::ipc) fn take_no_senders(&mut self) -> Option<(Reference<Port>, u32)> {
        if self.send_rights != 0 {
            return None;
        }
        let request = self.no_senders.take()?;
        Some((request, self.make_send_count))
    }

    /// Sets its no-senders request to `notify`, or none, as
    /// `ipc_port_nsrequest` does: gives the previous right, and the
    /// notification to send now, if there are no senders, `sync` doesn't
    /// outrun the make-send count and `notify` is there.
    #[expect(
        clippy::type_complexity,
        reason = "the previous right and the one to send"
    )]
    pub(in crate::ipc) fn set_no_senders_request(
        &mut self,
        sync: u32,
        notify: Option<Reference<Port>>,
    ) -> (Option<Reference<Port>>, Option<(Reference<Port>, u32)>) {
        let previous = self.no_senders.take();
        if self.send_rights == 0 && sync <= self.make_send_count && notify.is_some() {
            return (
                previous,
                notify.map(|notify| (notify, self.make_send_count)),
            );
        }
        self.no_senders = notify;
        (previous, None)
    }

    /// Sets its port-destroyed request to `notify`, or none, as
    /// `ipc_port_pdrequest` does; gives the previous right.
    pub(in crate::ipc) const fn set_port_destroyed_request(
        &mut self,
        notify: Option<Reference<Port>>,
    ) -> Option<Reference<Port>> {
        core::mem::replace(&mut self.port_destroyed, notify)
    }

    /// Takes its port-destroyed request, if any.
    pub(in crate::ipc) const fn take_port_destroyed(&mut self) -> Option<Reference<Port>> {
        self.port_destroyed.take()
    }

    /// Readies its port's receive right to be handed over, its port alive,
    /// as `ipc_port_clear_receiver` does but for the port to live on.
    ///
    /// # Panics
    ///
    /// If its port is dead. If it is in a set, or a thread waits to receive
    /// from it: not yet.
    pub(in crate::ipc) fn clear_receiver_limbo(&mut self) {
        assert!(self.active, "ipc_port_clear_receiver: a dead port");
        assert!(
            self.set.is_none(),
            "ipc_port_clear_receiver: ipc_pset_remove, not yet"
        );
        assert!(
            self.receivers.is_empty(),
            "ipc_mqueue_changed: thread_go, not yet"
        );
        self.receiver = Receiver::None;
        self.receiver_name = PortName::NULL;
        self.protected_payload = None;
        self.make_send_count = 0;
        self.sequence_number = 0;
    }

    /// Readies its port's receive right to be destroyed, its port alive, as
    /// `ipc_port_clear_receiver` does.
    ///
    /// # Panics
    ///
    /// If its port is dead. If a thread waits to receive from it: not yet.
    pub(in crate::ipc) fn clear_receiver(&mut self) {
        assert!(self.active, "ipc_port_clear_receiver: a dead port");
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
