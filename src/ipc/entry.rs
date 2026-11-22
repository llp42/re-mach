// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Entries: one name in a space, and the rights it holds.

use super::object::Object;
use super::port::port_name::PortName;
use crate::collections::radix_tree::Owner;
use crate::slab::{Cache, CacheBox};

/// An entry: a name, its rights, and the object they are for.
pub struct Entry {
    /// Its name.
    pub(super) name: PortName,
    /// Its rights, and their user references.
    pub(super) bits: EntryBits,
    /// The port or port set its rights are for.
    pub(super) object: Option<Object>,
    /// Its dead-name request; 0 for none.
    pub(super) request: u32,
}

// SAFETY: an entry has no interior mutability, and its port set's pointer
// is followed only under `unsafe`.
unsafe impl Sync for Entry {}

impl Entry {
    /// A new entry for `name` that holds no rights.
    pub(super) const fn new(name: PortName) -> Self {
        Self {
            name,
            bits: EntryBits {
                rights: Rights::empty(),
                user_references: 0,
                message_accepted: false,
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
    /// Whether it has a message-accepted request.
    pub(super) message_accepted: bool,
}

bitflags::bitflags! {
    /// The rights a name holds: bit n stands for Mach's type bit 16 + n.
    #[derive(Clone, Copy)]
    pub(super) struct Rights: u16 {
        /// A send right.
        const SEND = 1;
        /// The receive right.
        const RECEIVE = 1 << 1;
        /// A send-once right.
        const SEND_ONCE = 1 << 2;
        /// A port set's right.
        const PORT_SET = 1 << 3;
        /// A dead name.
        const DEAD_NAME = 1 << 4;
    }
}

/// The entry of name 0, which every space reserves.
static ZERO_ENTRY: Entry = Entry::new(PortName::NULL);

// SAFETY: an entry box gives the tree an entry nothing else points to,
// which may move between threads with its space; the zero entry is a
// static nothing writes.
unsafe impl Owner for CacheBox<Entry> {
    const SHARED: &'static Entry = &ZERO_ENTRY;
}

/// The cache entries come from.
pub(super) static CACHE: Cache<Entry> = Cache::new("ipc_entry");
