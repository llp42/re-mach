// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Page faults.

use super::Protection;
use super::map::Map;
use crate::arch::{self, Machine as _};
use crate::error::Error;

/// Resolves a fault on the page at `address` in `map`, which asked for
/// `access`; so far it looks the address up, then stops its CPU where it
/// looks the page up in the object.
///
/// # Errors
///
/// As `Map::lookup`.
///
/// # Panics
///
/// As `Map::lookup`.
#[expect(
    clippy::significant_drop_tightening,
    reason = "vm_fault_page takes the object locked"
)]
pub fn fault(map: &Map, address: usize, access: Protection) -> Result<(), Error> {
    let mut lookup = map.lookup(address, access)?;
    // A reference of its own keeps the object while the map changes.
    lookup.object.reference();
    lookup.state.begin_paging();
    arch::Current::halt()
}
