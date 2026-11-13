// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Port sets.

use intrusive_collections::LinkedList;

use super::kernel_message;
use super::object::{self, Object};
use super::port::PortName;
use super::space::Space;
use crate::error::Error;
use crate::slab::{Cache, CacheBox};
use crate::sync::Spinlock;
use crate::thread::IpcQueueAdapter;

/// A port set: its state under its lock.
pub struct PortSet {
    /// Its state.
    state: Spinlock<PortSetState>,
}

/// A port set's state under its lock: whether it is alive, its reference
/// count, its name, and its message queue.
#[expect(
    dead_code,
    reason = "ipc_pset_destroy and receiving from a set read them"
)]
struct PortSetState {
    /// Whether it is alive.
    active: bool,
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
    /// `object::allocate`.
    ///
    /// # Panics
    ///
    /// As `object::allocate`.
    pub(super) fn allocate(space: &Space) -> Result<PortName, Error> {
        let set = CACHE.allocate(Self::new())?;
        object::allocate(
            space,
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
    /// `object::allocate_name`.
    ///
    /// # Panics
    ///
    /// As `object::allocate_name`.
    pub(super) fn allocate_name(space: &Space, name: PortName) -> Result<(), Error> {
        let mut set = CACHE.allocate(Self::new())?;
        // As ipc_target_init: its name; `new` gave it an empty queue.
        set.state.get_mut().local_name = name;
        object::allocate_name(space, set, name, |set| {
            Object::PortSet(CacheBox::into_raw(set))
        })
    }
}

/// The cache port sets come from.
pub(super) static CACHE: Cache<PortSet> = Cache::new("ipc_pset");
