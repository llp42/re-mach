// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Port calls: what tasks ask of a space's names.

use core::{ptr, slice};

use super::MessageType;
use super::entry::{Entry, Rights};
use super::object::Object;
use super::port;
use super::port::port::Port;
use super::port::port_name::{PortName, PortType, Right};
use super::port::send_right::SendOnceRight;
use super::port_set::PortSet;
use super::space::{Space, SpaceState};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::reference::Reference;
use crate::slab::CacheBox;
use crate::vm::map::change::{Placement, RangeCheck, Source};
use crate::vm::map::copy::MapCopy;
use crate::vm::{self, Protection};

impl Entry {
    /// The type of its rights, its space locked, as
    /// `mach_port_names_helper` gives it.
    ///
    /// # Panics
    ///
    /// If its send right has no port. If that port is dead, or the entry has
    /// a dead-name or message-accepted request: not yet.
    fn port_type(&self) -> PortType {
        if self
            .bits
            .rights
            .intersects(Rights::SEND | Rights::SEND_ONCE)
        {
            let Some(Object::Port(port)) = &self.object else {
                panic!("mach_port_names_helper: a send right with no port");
            };
            let active = port.state.lock().active;
            assert!(active, "mach_port_names_helper: a dead port, not yet");
        }
        let port_type = PortType::from(self.bits.rights);
        assert!(
            self.request == 0,
            "mach_port_names_helper: MACH_PORT_TYPE_DNREQUEST, not yet"
        );
        assert!(
            !self.bits.message_accepted,
            "mach_port_names_helper: MACH_PORT_TYPE_MAREQUEST, not yet"
        );
        port_type
    }
}

impl Space {
    /// The names it holds rights under and their types, as
    /// `mach_port_names` lists them: each array moved into a copy, and how many
    /// elements both hold.
    ///
    /// # Panics
    ///
    /// If the walk finds at least as many names as the space's size. If the
    /// space is dead, no room is left in the IPC map, the space outgrew the
    /// arrays, holds no names or its names need less room than the arrays have:
    /// not yet. As `vm::user::allocate`, `Map::pageable`, `Entry::port_type`
    /// and `Map::copy_in`.
    pub fn names(&self) -> (CacheBox<MapCopy>, CacheBox<MapCopy>, usize) {
        let map = super::KERNEL_MAP
            .get()
            .expect("mach_port_names: no IPC map");
        // The arrays' size, and where they are once allocated.
        let mut size = 0;
        let mut arrays = None;
        // Allocated with the space unlocked, until they hold all its names.
        let (state, bound) = loop {
            let state = self.state.read();
            assert!(state.active, "mach_port_names: a dead space, not yet");
            // At most this many names.
            let bound = usize::try_from(state.size).unwrap();
            let size_needed = (bound * size_of::<PortName>()).next_multiple_of(PAGE_SIZE);
            if size_needed <= size {
                break (state, bound);
            }
            drop(state);
            assert!(arrays.is_none(), "mach_port_names: kmem_free, not yet");
            size = size_needed;
            let names = vm::user::allocate(map, Placement::Anywhere, size).unwrap_or_else(|_| {
                panic!("mach_port_names: no more room in ipc_kernel_map, not yet")
            });
            let types = vm::user::allocate(map, Placement::Anywhere, size).unwrap_or_else(|_| {
                panic!("mach_port_names: no more room in ipc_kernel_map, not yet")
            });
            // Wired, so filling them can't fault while the space is locked.
            for array in [names, types] {
                if let Err(error) = map.pageable(
                    array..array + size,
                    Protection::READ | Protection::WRITE,
                    RangeCheck::Checked,
                ) {
                    panic!("mach_port_names: vm_map_pageable failed: {error:?}");
                }
            }
            arrays = Some((names, types));
        };
        // Name 0 is in every space, so the first pass allocated them.
        let (names, types) = arrays.expect("mach_port_names: no arrays");
        // SAFETY: the arrays are two of this call's allocations, apart and
        // page-aligned, that nothing else reaches: `size` bytes each, readable
        // and writable, wired and zero-filled.
        let (name_array, type_array) = unsafe {
            (
                slice::from_raw_parts_mut(
                    ptr::with_exposed_provenance_mut::<PortName>(usize::from(names)),
                    size / size_of::<PortName>(),
                ),
                slice::from_raw_parts_mut(
                    ptr::with_exposed_provenance_mut::<PortType>(usize::from(types)),
                    size / size_of::<PortType>(),
                ),
            )
        };
        let mut actual = 0;
        // Read by the died branch, not yet.
        let _timestamp = port::port::timestamp();
        for entry in state.map.iter().filter(|entry| entry.in_use()) {
            let port_type = entry.port_type();
            name_array[actual] = entry.name;
            type_array[actual] = port_type;
            actual += 1;
        }
        assert!(
            actual < bound,
            "mach_port_names: at least as many names as the space's size"
        );
        drop(state);
        assert!(actual != 0, "mach_port_names: no names, not yet");
        let size_used = (actual * size_of::<PortName>()).next_multiple_of(PAGE_SIZE);
        // Pageable again, to be copied in.
        for array in [names, types] {
            if let Err(error) = map.pageable(
                array..array + size_used,
                Protection::empty(),
                RangeCheck::Checked,
            ) {
                panic!("mach_port_names: vm_map_pageable failed: {error:?}");
            }
        }
        // Copied in, the arrays leave the IPC map.
        let (names, types) = (
            map.copy_in(names..names + size_used, Source::Destroy),
            map.copy_in(types..types + size_used, Source::Destroy),
        );
        assert!(size_used == size, "mach_port_names: kmem_free, not yet");
        (names, types, actual)
    }

    /// The type of `name`'s rights in it, as `mach_port_type` gives it.
    ///
    /// # Panics
    ///
    /// As `Space::lookup_write` and `SpaceState::entry_info`.
    pub fn port_type(&self, name: PortName) -> PortType {
        let state = self.lookup_write(name);
        let entry = state.lookup(name).expect("mach_port_type: no entry");
        let (port_type, _) = state.entry_info(entry);
        drop(state);
        port_type
    }

    /// How many references `name` holds in it of the kind numbered
    /// `right`, as `mach_port_get_refs` counts them: one for a receive right
    /// or a port set, a dead name's user references.
    ///
    /// # Panics
    ///
    /// If a dead name holds no user references, or for a strange right. If
    /// `right` is no kind of right, a send or send-once right, or `name` holds
    /// no right of that kind: not yet. As `Space::lookup_write` and
    /// `SpaceState::entry_info`.
    pub fn user_references(&self, name: PortName, right: u32) -> u32 {
        // Mach has `MACH_PORT_RIGHT_NUMBER` kinds, five.
        assert!(right < 5, "mach_port_get_refs: KERN_INVALID_VALUE, not yet");
        let state = self.lookup_write(name);
        let entry = state.lookup(name).expect("mach_port_get_refs: no entry");
        let (port_type, user_references) = state.entry_info(entry);
        drop(state);
        // `MACH_PORT_TYPE(right)`.
        if port_type.contains(PortType::from(Rights::from_bits_retain(1 << right))) {
            match Right::try_from(right) {
                Ok(Right::Send) => u32::from(user_references),
                Ok(Right::Receive | Right::PortSet | Right::SendOnce) => 1,
                Ok(Right::DeadName) => {
                    assert!(
                        user_references > 0,
                        "mach_port_get_refs: no user references"
                    );
                    u32::from(user_references)
                }
                Err(right) => panic!("mach_port_get_refs: strange rights {right}"),
            }
        } else {
            panic!("mach_port_get_refs: no right of that kind, not yet");
        }
    }

    /// Destroys `name`'s rights in it, as `mach_port_destroy` does.
    ///
    /// # Panics
    ///
    /// As `Space::lookup_write` and `SpaceState::destroy_rights`.
    pub fn destroy_rights(&self, name: PortName) {
        let state = self.lookup_write(name);
        SpaceState::destroy_rights(state, self, name);
    }

    /// Releases a user reference of `name`'s right in it, as
    /// `mach_port_deallocate` does.
    ///
    /// # Panics
    ///
    /// As `Space::lookup_write` and `SpaceState::release_user_reference`.
    pub fn release_user_reference(&self, name: PortName) {
        let state = self.lookup_write(name);
        SpaceState::release_user_reference(state, name);
    }

    /// Puts a new right of the kind numbered `right` in it under a free
    /// name, as `mach_port_allocate` does, and gives the name: a receive right,
    /// a port set or a dead name.
    ///
    /// # Errors
    ///
    /// As `Port::allocate`, `PortSet::allocate` and `Space::insert_dead_name`.
    ///
    /// # Panics
    ///
    /// If `right` is none of those kinds: not yet. As `Port::allocate`,
    /// `PortSet::allocate` and `Space::insert_dead_name`.
    pub fn allocate(&self, right: u32) -> Result<PortName, Error> {
        let right = Right::try_from(right).unwrap_or_else(|right| {
            panic!("mach_port_allocate: KERN_INVALID_VALUE for right {right}, not yet")
        });
        match right {
            Right::Receive => Port::allocate(self),
            Right::PortSet => PortSet::allocate(self),
            Right::DeadName => self.insert_dead_name(),
            Right::Send | Right::SendOnce => Err(Error::InvalidValue),
        }
    }

    /// Puts a new right of the kind numbered `right` in it as `name`, as
    /// `mach_port_allocate_name` does: a receive right, a port set or a dead
    /// name.
    ///
    /// # Errors
    ///
    /// As `Port::allocate_name`, `PortSet::allocate_name` and
    /// `Space::insert_dead_name_as`.
    ///
    /// # Panics
    ///
    /// If `name` is null or dead, or `right` is none of those kinds: not yet.
    /// As `Port::allocate_name`, `PortSet::allocate_name` and
    /// `Space::insert_dead_name_as`.
    pub fn allocate_as(&self, right: u32, name: PortName) -> Result<(), Error> {
        assert!(
            name.is_valid(),
            "mach_port_allocate_name: KERN_INVALID_VALUE, not yet"
        );
        let right = Right::try_from(right).unwrap_or_else(|right| {
            panic!("mach_port_allocate_name: KERN_INVALID_VALUE for right {right}, not yet")
        });
        match right {
            Right::Receive => Port::allocate_name(self, name),
            Right::PortSet => PortSet::allocate_name(self, name),
            Right::DeadName => self.insert_dead_name_as(name),
            Right::Send | Right::SendOnce => Err(Error::InvalidValue),
        }
    }

    /// Inserts the right `message_type` names, for `port`, in it as `name`; it
    /// takes the right and its reference.
    ///
    /// # Errors
    ///
    /// As `Space::copy_out_as`, which gives the right back.
    ///
    /// # Panics
    ///
    /// If `name` is null or dead: not yet; as `Space::copy_out_as`.
    pub fn insert_right(
        &self,
        name: PortName,
        port: Reference<Port>,
        message_type: MessageType,
    ) -> Result<(), (Error, Reference<Port>)> {
        assert!(
            name.is_valid(),
            "mach_port_insert_right: an invalid name, not yet"
        );
        self.copy_out_as(port, message_type, name)
    }
}

/// Mach's `mach_port_status_t`, as its calls fill it.
#[derive(Clone, Copy)]
#[expect(dead_code, reason = "MIG's reply packs the record whole")]
pub struct Status {
    /// The set it is in, if any.
    pub pset: u32,
    /// The sequence number of the next message received from it.
    pub seqno: u32,
    /// How many send rights were made from its receive right.
    pub mscount: u32,
    /// How many messages it queues before senders wait.
    pub qlimit: u32,
    /// How many messages are queued on it.
    pub msgcount: u32,
    /// How many send-once rights for it there are.
    pub sorights: u32,
    /// Whether any send right for it exists.
    pub srights: u32,
    /// Whether a port-deleted notification is requested.
    pub pdrequest: u32,
    /// Whether a no-senders notification is requested.
    pub nsrequest: u32,
}

/// `MACH_PORT_QLIMIT_MAX`.
pub const MACH_PORT_QLIMIT_MAX: u32 = 16;

/// `MACH_PORT_KTYPE_USER_DEVICE`.
const MACH_PORT_KTYPE_USER_DEVICE: u32 = 28;

impl Space {
    /// Requests the notification `id` for `name`, `notify` its send-once
    /// right, as `mach_port_request_notification` does; gives the previous
    /// request's right.
    ///
    /// # Errors
    ///
    /// `InvalidTask` for a dead space, `InvalidValue` for another `id` or a
    /// port-destroyed request with `sync`, and as `receive_port`.
    ///
    /// # Panics
    ///
    /// For a dead name's dead-name request: not yet.
    pub fn request_notification(
        &self,
        name: PortName,
        id: i32,
        sync: u32,
        notify: Option<SendOnceRight>,
    ) -> Result<Option<Reference<Port>>, Error> {
        match id {
            super::notify::PORT_DESTROYED => {
                if sync != 0 {
                    return Err(Error::InvalidValue);
                }
                let port = self.receive_port(name)?;
                let mut state = port.state.lock();
                assert!(
                    state.active,
                    "mach_port_request_notification: a dead port, not yet"
                );
                let previous =
                    state.set_port_destroyed_request(notify.map(SendOnceRight::into_reference));
                drop(state);
                Ok(previous)
            }
            super::notify::NO_SENDERS => {
                let port = self.receive_port(name)?;
                let mut state = port.state.lock();
                let (previous, immediate) =
                    state.set_no_senders_request(sync, notify.map(SendOnceRight::into_reference));
                drop(state);
                if let Some((request, mscount)) = immediate {
                    super::notify::no_senders(request, mscount);
                }
                Ok(previous)
            }
            super::notify::DEAD_NAME => {
                let mut state = self.state.write();
                if !state.active {
                    return Err(Error::InvalidTask);
                }
                let entry = state.map.get_mut(name.key()).ok_or(Error::InvalidName)?;
                if entry.bits.rights.bits() == Rights::DEAD_NAME.bits() {
                    assert!(
                        sync != 0 && notify.is_some(),
                        "mach_port_request_notification: a dead name, KERN_INVALID_ARGUMENT, not yet"
                    );
                    let request = notify.unwrap().into_reference();
                    assert!(
                        entry.bits.user_references < u16::MAX,
                        "mach_port_request_notification: MACH_PORT_UREFS_MAX, not yet"
                    );
                    entry.bits.user_references += 1;
                    let dead_name = entry.name;
                    drop(state);
                    super::notify::dead_name(request, dead_name);
                    return Ok(None);
                }
                assert!(
                    entry
                        .bits
                        .rights
                        .intersects(Rights::SEND | Rights::SEND_ONCE | Rights::RECEIVE),
                    "mach_port_request_notification: an entry with no right, KERN_INVALID_RIGHT, not yet"
                );
                let previous = entry.dead_name.take();
                entry.dead_name = notify.map(SendOnceRight::into_reference);
                drop(state);
                Ok(previous)
            }
            _ => Err(Error::InvalidValue),
        }
    }

    /// The receive right `name` holds, with a reference, as
    /// `ipc_port_translate_receive` gives it.
    ///
    /// # Errors
    ///
    /// `InvalidTask` for a dead space, `InvalidName` if `name` denotes no
    /// right, and `InvalidRight` if it holds no receive right.
    ///
    /// # Panics
    ///
    /// If the entry has no port.
    pub(in crate::ipc) fn receive_port(&self, name: PortName) -> Result<Reference<Port>, Error> {
        let state = self.state.read();
        if !state.active {
            return Err(Error::InvalidTask);
        }
        let entry = state.lookup(name).ok_or(Error::InvalidName)?;
        if !entry.bits.rights.contains(Rights::RECEIVE) {
            return Err(Error::InvalidRight);
        }
        let Some(Object::Port(port)) = &entry.object else {
            panic!("ipc_port_translate_receive: a receive right with no port");
        };
        let port = port.clone();
        drop(state);
        Ok(port)
    }

    /// Sets the queue limit of `name`'s receive right, as
    /// `mach_port_set_qlimit` does.
    ///
    /// # Errors
    ///
    /// `InvalidValue` past `MACH_PORT_QLIMIT_MAX`; as `receive_port`.
    pub fn set_queue_limit(&self, name: PortName, qlimit: u32) -> Result<(), Error> {
        if qlimit > MACH_PORT_QLIMIT_MAX {
            return Err(Error::InvalidValue);
        }
        let port = self.receive_port(name)?;
        port.state.lock().queue_limit = qlimit;
        Ok(())
    }

    /// The status of `name`'s receive right, as
    /// `mach_port_get_receive_status` gives it.
    ///
    /// # Errors
    ///
    /// As `receive_port`.
    ///
    /// # Panics
    ///
    /// If the port or its set is dead: not yet.
    pub fn receive_status(&self, name: PortName) -> Result<Status, Error> {
        let port = self.receive_port(name)?;
        let state = port.state.lock();
        assert!(
            state.active,
            "mach_port_get_receive_status: a dead port, not yet"
        );
        let pset = state.set.map_or(PortName::NULL, |set| {
            // SAFETY: the port's reference keeps the set alive.
            let set = unsafe { set.as_ref() };
            let set_state = set.state.lock();
            assert!(
                set_state.active,
                "mach_port_get_receive_status: ipc_pset_remove, not yet"
            );
            set_state.local_name
        });
        Ok(Status {
            pset: pset.into(),
            seqno: state.sequence_number,
            mscount: state.make_send_count,
            qlimit: state.queue_limit,
            msgcount: state.message_count,
            sorights: state.send_once_rights,
            srights: u32::from(state.send_rights > 0),
            // No call requests notifications yet.
            pdrequest: 0,
            nsrequest: 0,
        })
    }

    /// Sets `name`'s receive right's protected payload, or clears it for
    /// none, as `mach_port_set_protected_payload` and
    /// `mach_port_clear_protected_payload` do.
    ///
    /// # Errors
    ///
    /// As `receive_port`.
    pub fn set_protected_payload(&self, name: PortName, payload: Option<u64>) -> Result<(), Error> {
        let port = self.receive_port(name)?;
        port.state.lock().protected_payload = payload;
        Ok(())
    }

    /// Extracts the right `message_type` names for `name` from it, as
    /// `mach_port_extract_right` does; gives the port's right.
    ///
    /// # Panics
    ///
    /// As `Space::copy_in`.
    pub fn extract_right(&self, name: PortName, message_type: MessageType) -> Reference<Port> {
        self.copy_in(name, message_type)
    }

    /// Makes `name`'s port stand for a user device, or nothing, as
    /// `mach_port_set_ktype` does; `right` numbers the kind of right it must
    /// hold, and `ktype` is `MACH_PORT_KTYPE_NONE` or
    /// `MACH_PORT_KTYPE_USER_DEVICE`.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for another kernel object or kind, `InvalidRight`
    /// if `name` does not hold the right `right` numbers or is no port, and
    /// as `receive_port`.
    pub fn set_ktype(&self, name: PortName, right: u32, ktype: u32) -> Result<(), Error> {
        let Ok(right) = Right::try_from(right) else {
            return Err(Error::InvalidRight);
        };
        let user_device = match ktype {
            0 => false,
            MACH_PORT_KTYPE_USER_DEVICE => true,
            _ => return Err(Error::InvalidArgument),
        };
        let state = self.state.read();
        if !state.active {
            return Err(Error::InvalidTask);
        }
        let entry = state.lookup(name).ok_or(Error::InvalidName)?;
        let held = match right {
            Right::Send => Rights::SEND,
            Right::Receive => Rights::RECEIVE,
            Right::SendOnce => Rights::SEND_ONCE,
            Right::PortSet => Rights::PORT_SET,
            Right::DeadName => Rights::DEAD_NAME,
        };
        if !entry.bits.rights.contains(held) {
            return Err(Error::InvalidRight);
        }
        let Some(Object::Port(port)) = &entry.object else {
            return Err(Error::InvalidRight);
        };
        let port = port.clone();
        drop(state);
        port.set_user_device(user_device)
    }
}
