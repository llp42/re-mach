// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Entries: one name in a space, and the rights it holds.

use core::ptr::NonNull;

use super::port::Port;
use crate::slab::Cache;

/// An entry: a name, its rights, and the port they are for.
pub struct Entry {
    /// Its name.
    pub(super) name: u32,
    /// Its rights, and their user references.
    pub(super) bits: EntryBits,
    /// The port its rights are for.
    pub(super) object: Option<NonNull<Port>>,
    /// Its dead-name request; 0 for none.
    #[expect(dead_code, reason = "the dead-name calls read it")]
    pub(super) request: u32,
}

// SAFETY: an entry has no interior mutability, and its port pointer is
// followed only under `unsafe`.
unsafe impl Sync for Entry {}

impl Entry {
    /// A new entry for `name` that holds no rights.
    pub(super) const fn new(name: u32) -> Self {
        Self {
            name,
            bits: EntryBits {
                rights: Rights::empty(),
                user_references: 0,
            },
            object: None,
            request: 0,
        }
    }

    /// Whether it holds rights.
    pub(super) const fn in_use(&self) -> bool {
        !self.bits.rights.is_empty()
    }
}

/// An entry's rights, and the user references they hold.
pub(super) struct EntryBits {
    /// Its rights, by kind.
    pub(super) rights: Rights,
    /// How many user references they hold.
    pub(super) user_references: u16,
}

bitflags::bitflags! {
    /// The rights a name holds: bit n stands for Mach's type bit 16 + n.
    pub(super) struct Rights: u16 {
        /// A send right.
        const SEND = 1;
        /// The receive right.
        const RECEIVE = 1 << 1;
    }
}

/// The entry of name 0, which every space reserves.
pub static ZERO_ENTRY: Entry = Entry::new(0);

/// The cache entries come from.
pub(super) static CACHE: Cache<Entry> = Cache::new("ipc_entry");
