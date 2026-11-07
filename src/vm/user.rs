// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The virtual memory calls of Mach's interface.

use super::map::{Change, Map};
use super::{Inheritance, Protection};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::ipc::port::Port;

/// Where a call puts the memory it maps.
#[derive(Clone, Copy)]
pub enum Placement {
    /// At the address given.
    Fixed,
    /// Wherever it fits.
    #[expect(dead_code, reason = "the vm_allocate RPC asks for it")]
    Anywhere,
}

/// Maps `size` bytes of zero-filled memory into `map`, as `placement`
/// says, from `address` rounded down to a page, `size` rounded up to
/// pages; returns where they start, or 0 for no bytes. They may be read
/// and written, at most anything, and child tasks get a copy.
///
/// # Errors
///
/// `InvalidArgument` if `size` rounds up past the last address,
/// `InvalidAddress` if the pages would end past it, and as `Map::enter`.
///
/// # Panics
///
/// For `Placement::Anywhere`: not yet. As `Map::enter`.
pub fn allocate(
    map: &Map,
    address: usize,
    size: usize,
    placement: Placement,
) -> Result<usize, Error> {
    if size == 0 {
        return Ok(0);
    }
    assert!(
        matches!(placement, Placement::Fixed),
        "vm_allocate: anywhere, not yet"
    );
    // No mask; VM_PROT_DEFAULT, VM_PROT_ALL and VM_INHERIT_DEFAULT.
    enter(
        map,
        address,
        size,
        0,
        Protection::READ | Protection::WRITE,
        Protection::all(),
        Inheritance::Copy,
    )
}

/// Maps `size` bytes of `memory_object`, or of zero-filled memory for none,
/// into `map`, as `placement` says, at an address with no bit of `mask`
/// set, from `address` rounded down to a page, `size` rounded up to pages;
/// returns where they start. They allow `protection`, at most
/// `max_protection`, and child tasks get `inheritance` of them.
///
/// # Errors
///
/// `InvalidArgument` if a protection holds more than reading, writing and
/// running, or `size` is 0 or rounds up past the last address, `NoSpace`
/// if the start has a bit of `mask` set, `InvalidAddress` if the pages
/// would end past the last address, and as `Map::enter`.
///
/// # Panics
///
/// For a memory object or `Placement::Anywhere`: not yet. As `Map::enter`.
#[expect(clippy::too_many_arguments, reason = "vm_map's parameters")]
pub fn map(
    map: &Map,
    address: usize,
    size: usize,
    mask: usize,
    placement: Placement,
    memory_object: Option<&Port>,
    protection: Protection,
    max_protection: Protection,
    inheritance: Inheritance,
) -> Result<usize, Error> {
    // `inheritance` needs no check: its type holds only valid values.
    if !Protection::all().contains(protection) || !Protection::all().contains(max_protection) {
        return Err(Error::InvalidArgument);
    }
    if size == 0 {
        return Err(Error::InvalidArgument);
    }
    // IP_NULL: no object, so no offset or copy.
    assert!(memory_object.is_none(), "vm_map: a memory object, not yet");
    assert!(
        matches!(placement, Placement::Fixed),
        "vm_map: anywhere, not yet"
    );
    enter(
        map,
        address,
        size,
        mask,
        protection,
        max_protection,
        inheritance,
    )
}

/// Maps `size` bytes with nothing behind them yet into `map`, from
/// `address` rounded down to a page, at a start with no bit of `mask` set,
/// `size` rounded up to pages, as `Map::enter`; returns where they start.
///
/// # Errors
///
/// `InvalidArgument` if `size` rounds up past the last address, `NoSpace`
/// if the start has a bit of `mask` set, `InvalidAddress` if the pages
/// would end past the last address, and as `Map::enter`.
///
/// # Panics
///
/// As `Map::enter`.
fn enter(
    map: &Map,
    address: usize,
    size: usize,
    mask: usize,
    protection: Protection,
    max_protection: Protection,
    inheritance: Inheritance,
) -> Result<usize, Error> {
    let start = address & !(PAGE_SIZE - 1);
    // Past the top, GNU Mach's rounding wraps to 0, which vm_map_enter refuses.
    let size = size
        .checked_next_multiple_of(PAGE_SIZE)
        .ok_or(Error::InvalidArgument)?;
    if start & mask != 0 {
        return Err(Error::NoSpace);
    }
    let end = start.checked_add(size).ok_or(Error::InvalidAddress)?;
    map.enter(start..end, protection, max_protection, inheritance)?;
    Ok(start)
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
    address: usize,
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
    let start = address & !(PAGE_SIZE - 1);
    let end = address.wrapping_add(size).wrapping_add(PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
    map.protect(start..end, protection, change)
}
