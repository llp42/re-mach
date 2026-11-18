// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The virtual memory calls of Mach's interface.

use super::map::{Change, Map, Placement};
use super::object::{self, Object, Offset};
use super::{Inheritance, Protection, VirtualAddress, Wire};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::ipc::port::{KernelObject, Port};
use crate::reference::Reference;

/// What `map` maps of a memory object.
pub enum Mapping {
    /// The object itself.
    Object,
    /// A copy of it.
    Copy,
}

/// Maps `size` bytes of zero-filled memory into `map`, as `placement`
/// says, a fixed address rounded down to a page, `size` rounded up to
/// pages; returns where they start, or 0 for no bytes. They may be read
/// and written, at most anything, and child tasks get a copy.
///
/// # Errors
///
/// `InvalidArgument` if `size` rounds up past the last address, and as
/// `Map::enter`.
///
/// # Panics
///
/// As `Map::enter`.
pub fn allocate(map: &Map, placement: Placement, size: usize) -> Result<VirtualAddress, Error> {
    if size == 0 {
        return Ok(VirtualAddress::from(0));
    }
    // No mask; VM_PROT_DEFAULT, VM_PROT_ALL and VM_INHERIT_DEFAULT.
    enter(
        map,
        placement,
        size,
        0,
        None,
        Offset::from(0),
        Protection::READ | Protection::WRITE,
        Protection::all(),
        Inheritance::Copy,
    )
}

/// Maps `size` bytes of `memory_object` from `offset`, as `mapping` says,
/// or of zero-filled memory for none, into `map`, as `placement` says, at
/// an address with no bit of `mask` set, a fixed one rounded down to a
/// page, `size` rounded up to pages; returns where they start. They allow
/// `protection`, at most `max_protection`, and child tasks get
/// `inheritance` of them.
///
/// # Errors
///
/// `InvalidArgument` if a protection holds more than reading, writing and
/// running, or `size` is 0 or rounds up past the last address, `NoSpace`
/// if the start has a bit of `mask` set, and as `Map::enter`.
///
/// # Panics
///
/// If `memory_object` stands for no memory object, or for `Mapping::Copy`
/// of one: not yet. As `object::enter`, `enter` and, if mapping it fails,
/// `Object::deallocate`.
#[expect(clippy::too_many_arguments, reason = "vm_map's parameters")]
pub fn map(
    map: &Map,
    placement: Placement,
    size: usize,
    mask: usize,
    memory_object: Option<&Reference<Port>>,
    offset: Offset,
    mapping: Mapping,
    protection: Protection,
    max_protection: Protection,
    inheritance: Inheritance,
) -> Result<VirtualAddress, Error> {
    // `inheritance` needs no check: its type holds only valid values.
    if !Protection::all().contains(protection) || !Protection::all().contains(max_protection) {
        return Err(Error::InvalidArgument);
    }
    if size == 0 {
        return Err(Error::InvalidArgument);
    }
    // Past the top, GNU Mach's rounding wraps to 0, which Map::enter refuses.
    let size = size.checked_next_multiple_of(PAGE_SIZE).unwrap_or(0);
    // IP_NULL: no object, so no offset or copy.
    let (object, offset, mapping) = memory_object.map_or_else(
        || (None, Offset::from(0), Mapping::Object),
        |port| {
            let object = object::enter(port, size)
                .unwrap_or_else(|| panic!("vm_map: memory_object_proxy_lookup, not yet"));
            (Some(object), offset, mapping)
        },
    );
    assert!(
        matches!(mapping, Mapping::Object),
        "vm_map: vm_object_copy_strategically, not yet"
    );
    enter(
        map,
        placement,
        size,
        mask,
        object,
        offset,
        protection,
        max_protection,
        inheritance,
    )
    .inspect_err(|_| {
        if let Some(object) = object {
            object.deallocate();
        }
    })
}

/// Maps `size` bytes of `object` from `offset`, or with nothing behind them
/// yet for none, into `map`, as `placement` says, a fixed address rounded
/// down to a page and with no bit of `mask` set, `size` rounded up to
/// pages, as `Map::enter`; returns where they start.
///
/// # Errors
///
/// `InvalidArgument` if `size` rounds up past the last address, `NoSpace`
/// if a fixed start has a bit of `mask` set, and as `Map::enter`.
///
/// # Panics
///
/// For a mask with `Placement::Anywhere`: not yet. As `Map::enter`.
#[expect(clippy::too_many_arguments, reason = "vm_map_enter's parameters")]
fn enter(
    map: &Map,
    placement: Placement,
    size: usize,
    mask: usize,
    object: Option<&'static Object>,
    offset: Offset,
    protection: Protection,
    max_protection: Protection,
    inheritance: Inheritance,
) -> Result<VirtualAddress, Error> {
    let placement = match placement {
        Placement::Fixed(address) => Placement::Fixed(address.align_down(PAGE_SIZE)),
        Placement::Anywhere => Placement::Anywhere,
    };
    // Past the top, GNU Mach's rounding wraps to 0, which vm_map_enter refuses.
    let size = size
        .checked_next_multiple_of(PAGE_SIZE)
        .ok_or(Error::InvalidArgument)?;
    if let Placement::Fixed(start) = placement
        && usize::from(start) & mask != 0
    {
        return Err(Error::NoSpace);
    }
    assert!(
        mask == 0 || matches!(placement, Placement::Fixed(_)),
        "vm_map_find_entry_anywhere: a mask, not yet"
    );
    let range = map.enter(
        placement,
        size,
        object,
        offset,
        protection,
        max_protection,
        inheritance,
    )?;
    Ok(range.start)
}

/// Sets what `size` bytes of `map` from `address`, rounded out to pages,
/// allow to `protection`, as `change` says.
///
/// # Errors
///
/// `InvalidArgument` if `protection` holds more than reading, writing,
/// running and `VM_PROT_NOTIFY`, and as `Map::protect`.
///
/// # Panics
///
/// If `protection` holds `VM_PROT_NOTIFY`: not yet. As `Map::protect`.
pub fn protect(
    map: &Map,
    address: VirtualAddress,
    size: usize,
    change: Change,
    protection: Protection,
) -> Result<(), Error> {
    /// `VM_PROT_NOTIFY`, which GNU Mach lets through.
    const NOTIFY: u32 = 0x10;
    if protection.bits() & !(Protection::all().bits() | NOTIFY) != 0 {
        return Err(Error::InvalidArgument);
    }
    assert!(
        Protection::all().contains(protection),
        "vm_protect: VM_PROT_NOTIFY, not yet"
    );
    // GNU Mach's rounding, which wraps past the top.
    let end = usize::from(address)
        .wrapping_add(size)
        .wrapping_add(PAGE_SIZE - 1)
        & !(PAGE_SIZE - 1);
    map.protect(
        address.align_down(PAGE_SIZE)..VirtualAddress::from(end),
        protection,
        change,
    )
}

/// Wires all of `map` as `flags` say, or unwires it all for none, if
/// `host` is the host's privileged port.
///
/// # Errors
///
/// `InvalidHost` unless `host` is the host's privileged port, alive,
/// `InvalidTask` for no map, `InvalidArgument` if `flags` hold a bit
/// `Wire` lacks, and as `Map::pageable_all`.
///
/// # Panics
///
/// As `Map::pageable_all`.
pub fn wire_all(host: &Port, map: Option<&Map>, flags: u32) -> Result<(), Error> {
    if !matches!(host.kernel_object(), Some(KernelObject::HostPrivileged(_))) {
        return Err(Error::InvalidHost);
    }
    let Some(map) = map else {
        return Err(Error::InvalidTask);
    };
    let Some(flags) = Wire::from_bits(flags) else {
        return Err(Error::InvalidArgument);
    };
    // Maps hold no projected buffers.
    map.pageable_all(flags)
}
