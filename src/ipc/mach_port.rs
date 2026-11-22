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
use super::port_set::PortSet;
use super::space::{Space, SpaceState};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::reference::Reference;
use crate::slab::CacheBox;
use crate::vm::map::{MapCopy, Placement, RangeCheck, Source};
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
                Ok(Right::PortSet | Right::Receive) => 1,
                Ok(Right::DeadName) => {
                    assert!(
                        user_references > 0,
                        "mach_port_get_refs: no user references"
                    );
                    u32::from(user_references)
                }
                Err(0) => panic!("mach_port_get_refs: MACH_PORT_RIGHT_SEND, not yet"),
                Err(2) => panic!("mach_port_get_refs: MACH_PORT_RIGHT_SEND_ONCE, not yet"),
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

    /// Moves the rights of `old_name` in it to `new_name`, as
    /// `mach_port_rename` does.
    ///
    /// # Errors
    ///
    /// As `Space::rename_entry`.
    ///
    /// # Panics
    ///
    /// If `new_name` is null or dead: not yet; as `Space::rename_entry`.
    pub fn rename(&self, old_name: PortName, new_name: PortName) -> Result<(), Error> {
        assert!(
            new_name.is_valid(),
            "mach_port_rename: KERN_INVALID_VALUE, not yet"
        );
        self.rename_entry(old_name, new_name)
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
