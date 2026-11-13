// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Spaces: a task's names for ports and port sets.

use core::ptr::NonNull;

use super::entry::{self, Entry, EntryBits, Rights};
use super::port::PortName;
use crate::collections::ArrayVec;
use crate::collections::radix_tree::RadixTree;
use crate::error::Error;
use crate::reference::{Count, Counted};
use crate::slab::{Cache, CacheBox};
use crate::sync::SleepLock;

/// A space: the names a task gives ports and port sets.
pub struct Space {
    /// How many references it has.
    references: Count,
    /// Its state.
    pub(super) state: SleepLock<SpaceState>,
}

/// A space's state under its lock.
pub(super) struct SpaceState {
    /// Whether it is in use; special spaces never are.
    pub(super) active: bool,
    /// Its entries, by name.
    pub(super) map: RadixTree<CacheBox<Entry>>,
    /// Its send rights' entries, by port, as the name map points to them.
    pub(super) reverse_map: RadixTree<NonNull<Entry>>,
    /// How many names it holds.
    pub(super) size: u32,
    /// Its free entries' names, the last freed on top, up to GNU Mach's
    /// limit; the entries stay in the name map.
    pub(super) free_names: ArrayVec<PortName, 64>,
}

impl Space {
    /// A special space: one reference, never in use.
    const fn special() -> Self {
        Self {
            references: Count::new(1),
            state: SleepLock::new(SpaceState {
                active: false,
                map: RadixTree::new(),
                reverse_map: RadixTree::new(),
                size: 0,
                free_names: ArrayVec::new(),
            }),
        }
    }

    /// A new space, in use, with two references; name 0 is reserved.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no space can be allocated.
    pub fn create() -> Result<CacheBox<Self>, Error> {
        let mut map = RadixTree::new();
        map.insert_shared(PortName::NULL.key())
            .map_err(|_| Error::ResourceShortage)?;
        CACHE.allocate(Self {
            references: Count::new(2),
            state: SleepLock::new(SpaceState {
                active: true,
                map,
                reverse_map: RadixTree::new(),
                size: 1,
                free_names: ArrayVec::new(),
            }),
        })
    }
}

// SAFETY: its count is its own, and it is never freed yet.
unsafe impl Counted for Space {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `space`, as `ipc_space_release` does at zero.
    ///
    /// # Panics
    ///
    /// Always: not yet.
    unsafe fn free(_space: NonNull<Self>) {
        panic!("ipc_space_release: is_free, not yet");
    }
}

impl SpaceState {
    /// The entry of `name`, if it holds rights, as `ipc_entry_lookup` finds
    /// it.
    pub(super) fn lookup(&self, name: PortName) -> Option<&Entry> {
        self.map.get(name.key()).filter(|entry| entry.in_use())
    }

    /// The lowest free name, and a new entry for it that holds no rights.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the entry or a map node can't be allocated.
    ///
    /// # Panics
    ///
    /// If the space is dead, or a free entry waits: not yet.
    pub(super) fn allocate(&mut self) -> Result<(PortName, &mut Entry), Error> {
        assert!(self.active, "ipc_entry_alloc: a dead space, not yet");
        assert!(self.free_names.is_empty(), "ipc_entry_get, not yet");
        let entry = entry::CACHE.allocate(Entry::new(PortName::NULL))?;
        let (key, entry) = self
            .map
            .insert_alloc(entry)
            .map_err(|_| Error::ResourceShortage)?;
        self.size += 1;
        let name = PortName::from_key(key);
        entry.name = name;
        Ok((name, entry))
    }

    /// The entry of `name`, which holds rights, else a new one for it.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the entry or a map node can't be allocated.
    ///
    /// # Panics
    ///
    /// If the space is dead, or the name's entry is free: not yet.
    pub(super) fn allocate_name(&mut self, name: PortName) -> Result<&mut Entry, Error> {
        assert!(self.active, "ipc_entry_alloc_name: a dead space, not yet");
        let key = name.key();
        if self.map.get(key).is_some() {
            // The zero entry, which is free, is never lent mutably.
            let Some(entry) = self.map.get_mut(key).filter(|entry| entry.in_use()) else {
                panic!("ipc_entry_alloc_name: a free entry, not yet");
            };
            return Ok(entry);
        }
        let entry = entry::CACHE.allocate(Entry::new(name))?;
        let entry = self
            .map
            .insert(key, entry)
            .map_err(|_| Error::ResourceShortage)?;
        self.size += 1;
        Ok(entry)
    }

    /// Frees the entry of `name`, which holds no port, as `ipc_entry_dealloc`
    /// does: it keeps its name, which goes on the free list.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, or its entry holds a port
    /// or a request. If the free list is full: not yet.
    pub(super) fn deallocate(&mut self, name: PortName) {
        assert!(self.active, "ipc_entry_dealloc: a dead space");
        let entry = self
            .map
            .get_mut(name.key())
            .expect("ipc_entry_dealloc: no entry");
        assert!(
            entry.object.is_none() && entry.request == 0,
            "ipc_entry_dealloc: an entry with a port or a request"
        );
        assert!(
            !self.free_names.is_full(),
            "ipc_entry_dealloc: rdxtree_remove and ie_free, not yet"
        );
        entry.bits = EntryBits {
            rights: Rights::empty(),
            user_references: 0,
            message_accepted: false,
        };
        self.free_names.push(name);
        self.size -= 1;
    }
}

/// The cache spaces come from.
pub(super) static CACHE: Cache<Space> = Cache::new("ipc_space");

/// The space of the ports the kernel receives; never in use.
pub static KERNEL_SPACE: Space = Space::special();

/// The space that marks ports receiving kernel replies; never in use.
#[expect(dead_code, reason = "kernel replies mark their ports with it")]
pub static REPLY_SPACE: Space = Space::special();
