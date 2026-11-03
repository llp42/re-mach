// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Spaces: a task's names for ports and port sets.

use crate::kernel::slab::Cache;

/// A space; so far only its reference count and whether it is in use.
#[expect(dead_code, reason = "ipc_space_create and the space calls use them")]
pub struct Space {
    /// How many references it has.
    references: u32,
    /// Whether it is in use; special spaces never are.
    active: bool,
}

/// The cache spaces come from.
pub(super) static CACHE: Cache<Space> = Cache::new("ipc_space");
