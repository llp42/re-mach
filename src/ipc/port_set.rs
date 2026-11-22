// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Port sets.

use intrusive_collections::LinkedList;

use super::kernel_message;
use super::object::Object;
use super::port::PortName;
use super::space::Space;
use crate::error::Error;
use crate::slab::{Cache, CacheBox};
use crate::sync::{Spinlock, SpinlockGuard};
use crate::thread::thread::IpcQueueAdapter;

/// A port set: its state under its lock.
pub struct PortSet {
    /// Its state.
    pub(super) state: Spinlock<PortSetState>,
}

/// A port set's state under its lock: whether it is alive, its reference
/// count, its name, and its message queue.
#[expect(
    dead_code,
    reason = "receiving from a set reads its name and its messages"
)]
pub(super) struct PortSetState {
    /// Whether it is alive.
    pub(super) active: bool,
    /// How many references it has.
    references: u32,
    /// Its name in the space that holds it.
    local_name: PortName,
    /// The messages queued on it, oldest first.
    messages: LinkedList<kernel_message::QueueAdapter>,
    /// The threads waiting to receive from it, latest first.
    receivers: LinkedList<IpcQueueAdapter>,
}

impl PortSet {
    /// A new live port set, with one reference and no name.
    const fn new() -> Self {
        Self {
            state: Spinlock::new(PortSetState {
                active: true,
                references: 1,
                local_name: PortName::NULL,
                messages: LinkedList::new(kernel_message::QueueAdapter::NEW),
                receivers: LinkedList::new(IpcQueueAdapter::NEW),
            }),
        }
    }

    /// A new port set in `space` under a free name, its one reference the
    /// entry's, as `ipc_pset_alloc` makes it; gives the name.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no port set can be allocated; as
    /// `Space::insert_object`.
    ///
    /// # Panics
    ///
    /// As `Space::insert_object`.
    pub(super) fn allocate(space: &Space) -> Result<PortName, Error> {
        let set = CACHE.allocate(Self::new())?;
        space.insert_object(
            set,
            // As ipc_target_init: its name; `new` gave it an empty queue.
            |set, name| set.state.get_mut().local_name = name,
            |set| Object::PortSet(CacheBox::into_raw(set)),
        )
    }

    /// A new port set in `space` as `name`, its one reference the entry's,
    /// as `ipc_pset_alloc_name` makes it.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no port set can be allocated; as
    /// `Space::insert_object_as`.
    ///
    /// # Panics
    ///
    /// As `Space::insert_object_as`.
    pub(super) fn allocate_name(space: &Space, name: PortName) -> Result<(), Error> {
        let mut set = CACHE.allocate(Self::new())?;
        // As ipc_target_init: its name; `new` gave it an empty queue.
        set.state.get_mut().local_name = name;
        space.insert_object_as(set, name, |set| Object::PortSet(CacheBox::into_raw(set)))
    }

    /// Destroys the port set of locked state `state`, alive, as
    /// `ipc_pset_destroy` does: it is dead and unlocked, and the caller's
    /// reference is released. Gives whether that was the last, as
    /// `ips_check_unlock` reads it after the release; the caller then frees
    /// the set, with nothing borrowing it.
    ///
    /// # Panics
    ///
    /// If it is dead. If a thread waits to receive from it: not yet.
    pub(super) fn destroy(mut state: SpinlockGuard<'_, PortSetState>) -> bool {
        assert!(state.active, "ipc_pset_destroy: a dead port set");
        state.active = false;
        // As ipc_mqueue_changed, under the queue's lock, the set's.
        assert!(
            state.receivers.is_empty(),
            "ipc_mqueue_changed: thread_go, not yet"
        );
        // ipc_target_terminate does nothing.
        // As ips_release: the caller's reference.
        state.references -= 1;
        let last = state.references == 0;
        drop(state);
        last
    }
}

/// The cache port sets come from.
pub(super) static CACHE: Cache<PortSet> = Cache::new("ipc_pset");
