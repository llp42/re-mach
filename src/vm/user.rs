// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The virtual memory calls of Mach's interface.

use super::map::Map;
use super::{Inheritance, Protection};
use crate::arch::PAGE_SIZE;
use crate::error::Error;

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
    let start = match placement {
        Placement::Fixed => address & !(PAGE_SIZE - 1),
        Placement::Anywhere => panic!("vm_allocate: anywhere, not yet"),
    };
    // Past the top, GNU Mach's rounding wraps to 0, which vm_map_enter refuses.
    let size = size
        .checked_next_multiple_of(PAGE_SIZE)
        .ok_or(Error::InvalidArgument)?;
    let end = start.checked_add(size).ok_or(Error::InvalidAddress)?;
    // VM_PROT_DEFAULT, VM_PROT_ALL and VM_INHERIT_DEFAULT.
    map.enter(
        start..end,
        Protection::READ | Protection::WRITE,
        Protection::all(),
        Inheritance::Copy,
    )?;
    Ok(start)
}
