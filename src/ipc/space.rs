// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Spaces: a task's names for ports and port sets.

use core::ptr::NonNull;
use core::sync::atomic::AtomicU32;

use super::entry::{self, Entry, EntryBits, Rights, ZERO_ENTRY};
use crate::collections::radix_tree::RadixTree;
use crate::error::Error;
use crate::slab::{Cache, CacheBox};
use crate::sync::SleepLock;

/// A space: the names a task gives ports and port sets.
pub struct Space {
    /// How many references it has.
    #[expect(dead_code, reason = "the space calls read it")]
    references: AtomicU32,
    /// Its state.
    pub(super) state: SleepLock<SpaceState>,
}

/// A space's state under its lock.
pub(super) struct SpaceState {
    /// Whether it is in use; special spaces never are.
    pub(super) active: bool,
    /// Its entries, by name.
    pub(super) map: RadixTree<Entry>,
    /// Its send rights' entries, by port.
    pub(super) reverse_map: RadixTree<Entry>,
    /// How many names it holds.
    pub(super) size: u32,
}

impl Space {
    /// A special space: one reference, never in use.
    const fn special() -> Self {
        Self {
            references: AtomicU32::new(1),
            state: SleepLock::new(SpaceState {
                active: false,
                map: RadixTree::new(),
                reverse_map: RadixTree::new(),
                size: 0,
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
        map.insert(0, NonNull::from(&ZERO_ENTRY))
            .map_err(|_| Error::ResourceShortage)?;
        CACHE.allocate(Self {
            references: AtomicU32::new(2),
            state: SleepLock::new(SpaceState {
                active: true,
                map,
                reverse_map: RadixTree::new(),
                size: 1,
            }),
        })
    }
}

impl SpaceState {
    /// The entry of `name`, if it holds rights, as `ipc_entry_lookup` finds
    /// it.
    pub(super) fn lookup(&self, name: u32) -> Option<&Entry> {
        self.map
            .lookup(u64::from(name))
            // SAFETY: the space's lock, held, guards its entries, and no
            // mutable reference to one is alive while the state is shared.
            .map(|entry| unsafe { entry.as_ref() })
            .filter(|entry| entry.in_use())
    }

    /// The lowest free name, and a new entry for it that holds no rights.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the entry or a map node can't be allocated.
    ///
    /// # Panics
    ///
    /// If the space is dead: not yet.
    pub(super) fn allocate(&mut self) -> Result<(u32, NonNull<Entry>), Error> {
        assert!(self.active, "ipc_entry_alloc: a dead space, not yet");
        // Entries are never freed yet, so there is no free list to take from.
        let entry = CacheBox::into_raw(entry::CACHE.allocate(Entry {
            name: 0,
            bits: EntryBits {
                rights: Rights::empty(),
                user_references: 0,
            },
            object: None,
            request: 0,
        })?);
        let Ok(key) = self.map.insert_alloc(entry) else {
            // SAFETY: it came from `into_raw` of an entry box, and the map
            // didn't take it.
            drop(unsafe { CacheBox::from_raw(&entry::CACHE, entry) });
            return Err(Error::ResourceShortage);
        };
        self.size += 1;
        let name = u32::try_from(key).unwrap();
        // SAFETY: the space's lock, held, guards its entries, and no
        // reference to this one is alive.
        unsafe { (*entry.as_ptr()).name = name };
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
    pub(super) fn allocate_name(&mut self, name: u32) -> Result<NonNull<Entry>, Error> {
        assert!(self.active, "ipc_entry_alloc_name: a dead space, not yet");
        let key = u64::from(name);
        if let Some(entry) = self.map.lookup(key) {
            // SAFETY: the space's lock, held, guards its entries, and no
            // reference to this one is alive.
            let used = unsafe { entry.as_ref() }.in_use();
            assert!(used, "ipc_entry_alloc_name: a free entry, not yet");
            return Ok(entry);
        }
        let entry = CacheBox::into_raw(entry::CACHE.allocate(Entry {
            name,
            bits: EntryBits {
                rights: Rights::empty(),
                user_references: 0,
            },
            object: None,
            request: 0,
        })?);
        if self.map.insert(key, entry).is_err() {
            // SAFETY: it came from `into_raw` of an entry box, and the map
            // didn't take it.
            drop(unsafe { CacheBox::from_raw(&entry::CACHE, entry) });
            return Err(Error::ResourceShortage);
        }
        self.size += 1;
        Ok(entry)
    }
}

/// The cache spaces come from.
pub(super) static CACHE: Cache<Space> = Cache::new("ipc_space");

/// The space of the ports the kernel receives; never in use.
pub static KERNEL_SPACE: Space = Space::special();

/// The space that marks ports receiving kernel replies; never in use.
#[expect(dead_code, reason = "kernel replies mark their ports with it")]
pub static REPLY_SPACE: Space = Space::special();
