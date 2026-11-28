// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Port sets.

use core::mem::size_of;
use core::ptr::{self, NonNull};
use core::slice;

use tailq::TailQueue;

use super::entry::Rights;
use super::kernel_message;
use super::object::Object;
use super::port::port::Port;
use super::port::port_name::PortName;
use super::port::state::PortState;
use super::space::Space;
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::ipc::KERNEL_MAP;
use crate::slab::{Cache, CacheBox};
use crate::sync::{Spinlock, SpinlockGuard};
use crate::thread::thread::IpcQueueAdapter;
use crate::vm;
use crate::vm::Protection;
use crate::vm::map::change::{Placement, RangeCheck, Source};
use crate::vm::map::copy::MapCopy;

/// A port set: its state under its lock.
pub struct PortSet {
    /// Its state.
    pub(super) state: Spinlock<PortSetState>,
}

/// A port set's state under its lock: whether it is alive, its reference
/// count, its name, and its message queue.
pub(super) struct PortSetState {
    /// Whether it is alive.
    pub(super) active: bool,
    /// How many references it has.
    references: u32,
    /// Its name in the space that holds it.
    pub(in crate::ipc) local_name: PortName,
    /// The messages queued on it, oldest first.
    pub(super) messages: TailQueue<'static, kernel_message::QueueAdapter>,
    /// How many messages are queued on it.
    pub(super) message_count: u32,
    /// How many messages it queues before senders wait.
    pub(super) queue_limit: u32,
    /// The sequence number of the next message received from it.
    pub(super) sequence_number: u32,
    /// The threads waiting to receive from it, latest first.
    pub(super) receivers: TailQueue<'static, IpcQueueAdapter>,
}

impl PortSet {
    /// A new live port set, with one reference and no name.
    const fn new() -> Self {
        Self {
            state: Spinlock::new(PortSetState {
                active: true,
                references: 1,
                local_name: PortName::NULL,
                messages: TailQueue::new(),
                // MACH_PORT_QLIMIT_DEFAULT.
                message_count: 0,
                queue_limit: 5,
                sequence_number: 0,
                receivers: TailQueue::new(),
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

impl PortSet {
    /// Whether it is alive.
    pub(in crate::ipc) fn active(&self) -> bool {
        self.state.lock().active
    }

    /// Takes a reference, as `ips_reference` does.
    pub(in crate::ipc) fn reference(&self) {
        self.state.lock().references += 1;
    }

    /// Releases a reference, as `ips_release` does; gives whether that was
    /// the last.
    pub(in crate::ipc) fn release(&self) -> bool {
        let mut state = self.state.lock();
        state.references -= 1;
        state.references == 0
    }

    /// Moves the messages `port` has queued on it to it, as `ipc_pset_add`
    /// does on a move into the set; both the port and the set are locked.
    ///
    /// # Panics
    ///
    /// If the port has messages under other destinations: not yet.
    pub(super) fn take_port_messages(&self, port: &Port, port_state: &mut PortState) {
        let mut state = self.state.lock();
        while let Some(message) = kernel_message::KernelMessage::dequeue(&mut port_state.messages) {
            assert!(
                message.header().remote_port == ptr::from_ref(port).addr(),
                "ipc_mqueue_move: a message for another port, not yet"
            );
            port_state.message_count -= 1;
            state.message_count += 1;
            message.enqueue(&mut state.messages);
        }
        drop(state);
    }

    /// Moves back the messages queued on it for `port`, as `ipc_pset_remove`
    /// does on a move out of the set; both the port and the set are locked.
    pub(super) fn return_port_messages(&self, port: &Port, port_state: &mut PortState) {
        let mut state = self.state.lock();
        let mut moved = 0;
        let mut cursor = state.messages.cursor_front_mut();
        while let Some(message) = cursor.current() {
            if message.header().remote_port == ptr::from_ref(port).addr() {
                let message = kernel_message::KernelMessage::from_queued(
                    cursor
                        .remove_current_ptr()
                        .expect("ipc_mqueue_move: no message"),
                );
                moved += 1;
                port_state.message_count += 1;
                message.enqueue(&mut port_state.messages);
            } else {
                cursor.move_next();
            }
        }
        state.message_count -= moved;
        drop(state);
    }
}

impl Space {
    /// Moves `member`'s port into the set `after` names, or out of its set
    /// for `MACH_PORT_NULL`, as `mach_port_move_member` does.
    ///
    /// # Errors
    ///
    /// `InvalidTask` for a dead space, `InvalidName` if `member` or `after`
    /// denotes no right, `InvalidRight` if `member` holds no receive right
    /// or `after` no port set, and `NotInSet` if `after` is
    /// `MACH_PORT_NULL` and `member` is in no set.
    ///
    /// # Panics
    ///
    /// As `Port::move_to_set`.
    pub fn move_member(&self, member: PortName, after: PortName) -> Result<(), Error> {
        let state = self.state.read();
        if !state.active {
            return Err(Error::InvalidTask);
        }
        let entry = state.lookup(member).ok_or(Error::InvalidName)?;
        if !entry.bits.rights.contains(Rights::RECEIVE) {
            return Err(Error::InvalidRight);
        }
        let Some(Object::Port(port)) = &entry.object else {
            panic!("mach_port_move_member: a receive right with no port");
        };
        let port = port.clone();
        let set = if after == PortName::NULL {
            None
        } else {
            let entry = state.lookup(after).ok_or(Error::InvalidName)?;
            if !entry.bits.rights.contains(Rights::PORT_SET) {
                return Err(Error::InvalidRight);
            }
            let Some(Object::PortSet(set)) = &entry.object else {
                panic!("mach_port_move_member: a port set right with no set");
            };
            Some(*set)
        };
        // The space stays read-locked, which keeps `set`'s entry's
        // reference to it.
        let result = port.move_to_set(set);
        drop(state);
        result
    }

    /// The names of the members of the port set `name` names, as
    /// `mach_port_get_set_status` lists them: the copy holding them, or
    /// none for an empty set, and how many.
    ///
    /// # Errors
    ///
    /// `InvalidTask` for a dead space, `InvalidName` if `name` denotes no
    /// right, and `InvalidRight` if it holds no port set.
    ///
    /// # Panics
    ///
    /// If the space holds at least as many receive rights as its size, the
    /// IPC map has no room, or the names need another array: not yet. As
    /// `vm::user::allocate` and `Map::pageable`.
    pub fn set_members(&self, name: PortName) -> Result<(Option<CacheBox<MapCopy>>, usize), Error> {
        let map = KERNEL_MAP
            .get()
            .expect("mach_port_get_set_status: no IPC map");
        let mut size = 0;
        let mut array = None;
        let (state, set, bound) = loop {
            let state = self.state.read();
            if !state.active {
                return Err(Error::InvalidTask);
            }
            let entry = state.lookup(name).ok_or(Error::InvalidName)?;
            if !entry.bits.rights.contains(Rights::PORT_SET) {
                return Err(Error::InvalidRight);
            }
            let Some(Object::PortSet(set)) = &entry.object else {
                panic!("mach_port_get_set_status: a port set right with no set");
            };
            let set = *set;
            let bound = usize::try_from(state.size).unwrap();
            let needed = (bound * size_of::<PortName>()).next_multiple_of(PAGE_SIZE);
            if needed <= size {
                break (state, set, bound);
            }
            drop(state);
            assert!(
                array.is_none(),
                "mach_port_get_set_status: a growing space, not yet"
            );
            size = needed;
            let start = vm::user::allocate(map, Placement::Anywhere, size).unwrap_or_else(|_| {
                panic!("mach_port_get_set_status: no more room in ipc_kernel_map, not yet")
            });
            if let Err(error) = map.pageable(
                start..start + size,
                Protection::READ | Protection::WRITE,
                RangeCheck::Checked,
            ) {
                panic!("mach_port_get_set_status: vm_map_pageable failed: {error:?}");
            }
            array = Some(start);
        };
        // The zero entry makes the space's size at least one, so the array
        // was allocated.
        let start = array.expect("mach_port_get_set_status: no array");
        // SAFETY: the array is this call's allocation, `size` bytes,
        // page-aligned, wired and readable and writable; nothing else
        // reaches it.
        let members = unsafe {
            slice::from_raw_parts_mut(
                ptr::with_exposed_provenance_mut::<PortName>(usize::from(start)),
                size / size_of::<PortName>(),
            )
        };
        let mut actual = 0;
        for entry in state
            .map
            .iter()
            .filter(|entry| entry.in_use() && entry.bits.rights.contains(Rights::RECEIVE))
        {
            let Some(Object::Port(port)) = &entry.object else {
                panic!("mach_port_get_set_status: a receive right with no port");
            };
            let port_state = port.state.lock();
            let member = (port_state.set == Some(set)).then_some(port_state.receiver_name);
            drop(port_state);
            if let Some(name) = member {
                members[actual] = name;
                actual += 1;
            }
        }
        assert!(
            actual < bound,
            "mach_port_get_set_status: at least as many members as the space's size, not yet"
        );
        drop(state);
        if actual == 0 {
            // The array leaves the IPC map.
            vm::user::deallocate(map, start, size);
            return Ok((None, 0));
        }
        let used = (actual * size_of::<PortName>()).next_multiple_of(PAGE_SIZE);
        if let Err(error) = map.pageable(
            start..start + used,
            Protection::empty(),
            RangeCheck::Checked,
        ) {
            panic!("mach_port_get_set_status: vm_map_pageable failed: {error:?}");
        }
        let copy = map.copy_in(start..start + used, Source::Destroy);
        assert!(used == size, "mach_port_get_set_status: kmem_free, not yet");
        Ok((Some(copy), actual))
    }
}

impl Port {
    /// Moves it into `set`, or out of its set for none, as `ipc_pset_move`
    /// does, the space read-locked.
    ///
    /// # Errors
    ///
    /// `NotInSet` if `set` is none and it is in none, or its old set was
    /// destroyed.
    ///
    /// # Panics
    ///
    /// If it is dead, or it queues messages or has receivers waiting: not
    /// yet.
    pub(in crate::ipc) fn move_to_set(&self, set: Option<NonNull<PortSet>>) -> Result<(), Error> {
        let mut state = self.state.lock();
        assert!(state.active, "ipc_pset_move: a dead port");
        let old = state.set;
        if old == set {
            return Ok(());
        }
        assert!(
            state.receivers.is_empty(),
            "ipc_pset_move: waking receivers, not yet"
        );
        if old.is_none() && set.is_none() {
            return Err(Error::NotInSet);
        }
        if let Some(set) = set {
            // SAFETY: the set's entry keeps it alive while the space is
            // read-locked.
            let set = unsafe { set.as_ref() };
            set.reference();
            set.take_port_messages(self, &mut state);
        }
        if let Some(old) = old {
            // SAFETY: this port's reference kept the set alive.
            let old = unsafe { old.as_ref() };
            old.return_port_messages(self, &mut state);
        }
        state.set = set;
        drop(state);
        let Some(old) = old else {
            return Ok(());
        };
        // SAFETY: this port's reference kept the set alive until now.
        let old = unsafe { old.as_ref() };
        let active = old.active();
        let last = old.release();
        if last {
            // SAFETY: the last reference was this port's, and the set's box
            // came from its cache; nothing uses it after.
            drop(unsafe { CacheBox::from_raw(&CACHE, NonNull::from(old)) });
        }
        if active { Ok(()) } else { Err(Error::NotInSet) }
    }
}
