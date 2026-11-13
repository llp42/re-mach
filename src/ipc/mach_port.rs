// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Port calls: what tasks ask of a space's names.

use super::port::{Port, PortName};
use super::space::Space;
use super::{MessageType, object};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::slab::CacheBox;
use crate::vm::map::{MapCopy, Placement};
use crate::vm::{self, Protection};

/// The names `space` holds rights under and their types, as
/// `mach_port_names` lists them: each array moved into a copy, and how many
/// elements both hold.
///
/// # Panics
///
/// If the space is dead, no room is left in the IPC map, or the space
/// outgrew the arrays: not yet. As `vm::user::allocate` and
/// `Map::pageable`. Always, at the space's walk: not yet.
pub fn names(space: &Space) -> (CacheBox<MapCopy>, CacheBox<MapCopy>, usize) {
    let map = super::KERNEL_MAP
        .get()
        .expect("mach_port_names: no IPC map");
    // The arrays' size, and where they are once allocated.
    let mut size = 0;
    let mut arrays = None;
    // Allocated with the space unlocked, until they hold all its names.
    let _state = loop {
        let state = space.state.read();
        assert!(state.active, "mach_port_names: a dead space, not yet");
        // At most this many names.
        let bound = usize::try_from(state.size).unwrap();
        let size_needed = (bound * size_of::<PortName>()).next_multiple_of(PAGE_SIZE);
        if size_needed <= size {
            break state;
        }
        drop(state);
        assert!(arrays.is_none(), "mach_port_names: kmem_free, not yet");
        size = size_needed;
        let names = vm::user::allocate(map, Placement::Anywhere, size)
            .unwrap_or_else(|_| panic!("mach_port_names: no more room in ipc_kernel_map, not yet"));
        let types = vm::user::allocate(map, Placement::Anywhere, size)
            .unwrap_or_else(|_| panic!("mach_port_names: no more room in ipc_kernel_map, not yet"));
        // Wired, so filling them can't fault while the space is locked.
        for array in [names, types] {
            if let Err(error) =
                map.pageable(array..array + size, Protection::READ | Protection::WRITE)
            {
                panic!("mach_port_names: vm_map_pageable failed: {error:?}");
            }
        }
        arrays = Some((names, types));
    };
    panic!("mach_port_names: ipc_port_timestamp and the space's walk, not yet");
}

/// Inserts the right `message_type` names, for `port`, in `space` as
/// `name`; the space takes the right and its reference.
///
/// # Errors
///
/// As `object::copy_out_name`.
///
/// # Panics
///
/// If `name` is null or dead: not yet; as `object::copy_out_name`.
pub fn insert_right(
    space: &Space,
    name: PortName,
    port: &Port,
    message_type: MessageType,
) -> Result<(), Error> {
    assert!(
        name.is_valid(),
        "mach_port_insert_right: an invalid name, not yet"
    );
    object::copy_out_name(space, port, message_type, name)
}
