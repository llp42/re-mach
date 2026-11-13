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
use crate::vm;
use crate::vm::map::{MapCopy, Placement};

/// The names `space` holds rights under and their types, as
/// `mach_port_names` lists them: each array moved into a copy, and how many
/// elements both hold.
///
/// # Panics
///
/// If the space is dead, or no room is left in the IPC map: not yet. As
/// `vm::user::allocate`. Always, at wiring the arrays: not yet.
pub fn names(space: &Space) -> (CacheBox<MapCopy>, CacheBox<MapCopy>, usize) {
    let state = space.state.read();
    assert!(state.active, "mach_port_names: a dead space, not yet");
    // At most this many names.
    let bound = usize::try_from(state.size).unwrap();
    let size = (bound * size_of::<PortName>()).next_multiple_of(PAGE_SIZE);
    drop(state);
    let map = super::KERNEL_MAP
        .get()
        .expect("mach_port_names: no IPC map");
    let names = vm::user::allocate(map, Placement::Anywhere, size)
        .unwrap_or_else(|_| panic!("mach_port_names: no more room in ipc_kernel_map, not yet"));
    let types = vm::user::allocate(map, Placement::Anywhere, size)
        .unwrap_or_else(|_| panic!("mach_port_names: no more room in ipc_kernel_map, not yet"));
    panic!(
        "mach_port_names: vm_map_pageable of {:#x} and {:#x}, not yet",
        usize::from(names),
        usize::from(types)
    );
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
