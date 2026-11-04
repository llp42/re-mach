// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Entries: one name in a space, and the rights it holds.

use crate::kernel::slab::Cache;

/// An entry; so far only its user references.
#[expect(dead_code, reason = "ipc_entry_alloc and the right calls use it")]
pub struct Entry {
    /// How many user references its rights hold.
    user_references: u16,
}

/// The entry of name 0, which every space reserves.
pub static ZERO_ENTRY: Entry = Entry { user_references: 0 };

/// The cache entries come from.
pub(super) static CACHE: Cache<Entry> = Cache::new("ipc_entry");
