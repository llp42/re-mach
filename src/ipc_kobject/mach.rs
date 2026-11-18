// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Mach's calls' server routines, as MIG makes them from `mach`'s
//! definitions; only those written so far.

use super::mig::{self, Boolean};
use crate::error::Error;
use crate::ipc::port::Port;
use crate::reference::Reference;
use crate::task;
use crate::vm::map::{Map, Placement};
use crate::vm::object::Offset;
use crate::vm::user::Mapping;
use crate::vm::{self, Inheritance, Protection, VirtualAddress};

mig::subsystem! {
    base 2000;
    21 vm_allocate(
        target_task = task::map_of,
        address: VirtualAddress,
        size: usize,
        anywhere: Boolean
    ) -> (address: VirtualAddress);
    23 vm_deallocate(target_task = task::map_of, address: VirtualAddress, size: usize);
    89 vm_map(
        target_task = task::map_of,
        address: VirtualAddress,
        size: usize,
        mask: usize,
        anywhere: Boolean,
        memory_object: Port,
        offset: Offset,
        copy: Boolean,
        cur_protection: u32,
        max_protection: u32,
        inheritance: u32
    ) -> (address: VirtualAddress);
}

/// Serves `vm_allocate`: `size` bytes of zero-filled memory mapped into
/// `map` at `address` or anywhere; gives where.
///
/// # Errors
///
/// `InvalidArgument` for no map; as `vm::user::allocate`.
///
/// # Panics
///
/// As `vm::user::allocate`.
fn vm_allocate(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    size: usize,
    anywhere: Boolean,
) -> Result<VirtualAddress, Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    let placement = if bool::from(anywhere) {
        Placement::Anywhere
    } else {
        Placement::Fixed(address)
    };
    vm::user::allocate(map, placement, size)
}

/// Serves `vm_deallocate`: `size` bytes of `map` from `address` removed.
///
/// # Errors
///
/// `InvalidArgument` for no map.
///
/// # Panics
///
/// As `vm::user::deallocate`.
fn vm_deallocate(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    size: usize,
) -> Result<(), Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    vm::user::deallocate(map, address, size);
    Ok(())
}

/// Serves `vm_map`: `size` bytes of `memory_object` from `offset`, or a
/// copy of them, mapped into `map` at `address` or anywhere, with no bit of
/// `mask` set; gives where.
///
/// # Errors
///
/// `InvalidArgument` for no map or an inheritance Mach lacks; as
/// `vm::user::map`.
///
/// # Panics
///
/// For `VM_INHERIT_SHARE`: not yet. As `vm::user::map`.
#[expect(clippy::too_many_arguments, reason = "vm_map's parameters")]
fn vm_map(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    size: usize,
    mask: usize,
    anywhere: Boolean,
    memory_object: Option<&Reference<Port>>,
    offset: Offset,
    copy: Boolean,
    protection: u32,
    max_protection: u32,
    inheritance: u32,
) -> Result<VirtualAddress, Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    // VM_INHERIT_SHARE, VM_INHERIT_COPY, VM_INHERIT_NONE.
    let inheritance = match inheritance {
        0 => panic!("vm_map: VM_INHERIT_SHARE, not yet"),
        1 => Inheritance::Copy,
        2 => Inheritance::None,
        _ => return Err(Error::InvalidArgument),
    };
    let placement = if bool::from(anywhere) {
        Placement::Anywhere
    } else {
        Placement::Fixed(address)
    };
    let mapping = if bool::from(copy) {
        Mapping::Copy
    } else {
        Mapping::Object
    };
    vm::user::map(
        map,
        placement,
        size,
        mask,
        memory_object,
        offset,
        mapping,
        Protection::from_bits_retain(protection),
        Protection::from_bits_retain(max_protection),
        inheritance,
    )
}
