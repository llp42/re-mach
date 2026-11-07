// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Page faults.

use super::Protection;
use super::map::Map;
use crate::arch::{self, Machine as _};
use crate::error::Error;

/// Resolves a fault on the page at `address` in `map`, which asked for
/// `access`; so far it stops its CPU where it looks the address up.
#[expect(
    unused_variables,
    clippy::needless_pass_by_value,
    reason = "vm_map_lookup takes them"
)]
pub fn fault(map: &Map, address: usize, access: Protection) -> Result<(), Error> {
    arch::Current::halt()
}
