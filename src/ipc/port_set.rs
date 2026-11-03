// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Port sets.

use crate::kernel::slab::Cache;

/// A port set; so far only its reference count and whether it is alive.
#[expect(
    dead_code,
    reason = "mach_port_allocate and the port set calls use them"
)]
pub struct PortSet {
    /// How many references it has.
    references: u32,
    /// Whether it is alive.
    active: bool,
}

/// The cache port sets come from.
pub(super) static CACHE: Cache<PortSet> = Cache::new("ipc_pset");
