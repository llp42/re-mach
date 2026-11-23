// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU32, Ordering};

use super::allocate::CACHE;
use super::state::PortState;
use crate::ipc::kernel_message;
use crate::ipc::space::Space;
use crate::reference::{Count, Counted, Reference};
use crate::slab::CacheBox;
use crate::sync::{Spinlock, SpinlockGuard};
/// A port: its reference count, and its state under its lock.
pub struct Port {
    /// How many references it has.
    pub(super) references: Count,
    /// Its state.
    pub(in crate::ipc) state: Spinlock<PortState>,
}

/// A logical time that orders port deaths, Mach's `ipc_port_timestamp_t`.
#[derive(Clone, Copy, PartialEq)]
pub(in crate::ipc) struct Timestamp(u32);

/// Who holds a port's receive right, Mach's `data` union (`ip_receiver`,
/// `ip_timestamp`); only the kinds used here.
#[derive(Clone, Copy, PartialEq)]
pub(in crate::ipc) enum Receiver {
    /// No one, while the receive right is handed over.
    None,
    /// A space.
    Space(NonNull<Space>),
    /// No one: the port died at that time, Mach's `ip_timestamp`.
    Dead(Timestamp),
}

/// The next timestamp, in place of `ipc_port_timestamp_lock` and its data.
static TIMESTAMP: AtomicU32 = AtomicU32::new(0);

/// A new timestamp, as `ipc_port_timestamp` gives it.
pub(in crate::ipc) fn timestamp() -> Timestamp {
    Timestamp(TIMESTAMP.fetch_add(1, Ordering::Relaxed))
}

impl Port {
    /// Takes it out of its port set, if it is in one, with `state` locked,
    /// as `ipc_pset_remove` does: its queued messages go back to it, and the
    /// set's reference goes.
    pub(in crate::ipc) fn leave_set(&self, state: &mut PortState) {
        let Some(pointer) = state.set.take() else {
            return;
        };
        // SAFETY: this port's reference keeps its set alive until the
        // release below.
        let set = unsafe { pointer.as_ref() };
        set.return_port_messages(self, state);
        let active = set.active();
        let last = set.release();
        if last {
            // SAFETY: the last reference was this port's, and the set's box
            // came from its cache; nothing uses it after.
            drop(unsafe { CacheBox::from_raw(&crate::ipc::port_set::CACHE, NonNull::from(set)) });
        }
        assert!(active, "ipc_port_clear_receiver: a dead port set");
    }

    /// Destroys the port, alive, cleared of its receiver and of locked state
    /// `state`, as `ipc_port_destroy` does: it is dead and unlocked, and its
    /// queued messages are destroyed. The caller then drops the reference
    /// GNU Mach's call consumes.
    ///
    /// # Panics
    ///
    /// If it is dead, in a set, its make-send count or sequence number is
    /// not 0, a thread waits to receive from it, or a queued message is for
    /// another port. If it has a kernel object: not yet. As
    /// `KernelMessage::destroy`.
    pub(in crate::ipc) fn destroy(&self, mut state: SpinlockGuard<'_, PortState>) {
        assert!(state.active, "ipc_port_destroy: a dead port");
        // Out of its set, if it is in one, as ipc_port_clear_receiver does.
        self.leave_set(&mut state);
        assert!(
            state.set.is_none() && state.make_send_count == 0 && state.sequence_number == 0,
            "ipc_port_destroy: a receiver not cleared"
        );
        // Port-destroyed, no-senders and dead-name requests and blocked
        // senders come with the calls that make them, and their checks too.
        state.active = false;
        state.receiver = Receiver::Dead(timestamp());
        drop(state);
        // The queue's lock is the port's.
        let mut state = self.state.lock();
        assert!(
            state.receivers.is_empty(),
            "ipc_port_destroy: a thread receiving"
        );
        // Each queued message is destroyed with the port unlocked, its
        // destination's reference released first.
        while let Some(mut message) = kernel_message::KernelMessage::dequeue(&mut state.messages) {
            state.message_count -= 1;
            drop(state);
            let header = message.header_mut();
            assert!(
                header.remote_port == ptr::from_ref(self).addr(),
                "ipc_port_destroy: a message for another port"
            );
            // SAFETY: a queued message's destination slot holds a reference to
            // its port, this one, given up by `into_raw`, which the handle
            // takes; the caller's reference keeps the port alive after it.
            drop(unsafe { Reference::from_raw(NonNull::from(self)) });
            header.remote_port = 0;
            message.destroy();
            state = self.state.lock();
        }
        assert!(
            state.kernel_object.is_none(),
            "ipc_port_destroy: ipc_kobject_destroy, not yet"
        );
        drop(state);
        // ipc_target_terminate does nothing.
    }
}

// SAFETY: its count is its own, and every port is a box of `CACHE`, which
// `free` takes back.
unsafe impl Counted for Port {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `port`, as `ipc_port_release` does at zero: `io_check_unlock`
    /// frees it to its cache.
    unsafe fn free(port: NonNull<Self>) {
        // SAFETY: every port is a `CACHE` box that `From<CacheBox<Port>>`
        // gave up with `into_raw`, and nothing uses this one after.
        drop(unsafe { CacheBox::from_raw(&CACHE, port) });
    }
}

impl From<CacheBox<Port>> for Reference<Port> {
    /// The handle of the reference `port`'s box holds.
    fn from(port: CacheBox<Port>) -> Self {
        // SAFETY: a port's box holds its first reference, given up here.
        unsafe { Self::from_raw(CacheBox::into_raw(port)) }
    }
}
