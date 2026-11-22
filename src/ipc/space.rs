// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Spaces: a task's names for ports and port sets.

use core::mem;
use core::ptr::NonNull;

use super::entry::{self, Entry, EntryBits, Rights};
use super::object::Object;
use super::port::PortName;
use crate::collections::ArrayVec;
use crate::collections::radix_tree::RadixTree;
use crate::error::Error;
use crate::reference::{Count, Counted, Reference};
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

    /// Marks it dead and empties it, as `ipc_space_destroy` does; nothing if
    /// it is dead already. Releases its reference for being in use; the
    /// caller keeps its own.
    ///
    /// # Panics
    ///
    /// If it holds an entry besides the zero entry: not yet.
    pub fn destroy(&self) {
        let mut state = self.state.write();
        if !mem::replace(&mut state.active, false) {
            return;
        }
        // Each entry but the zero entry has its rights cleaned and is freed.
        if let Some(entry) = state.map.iter().find(|entry| entry.name != PortName::NULL) {
            assert!(
                !entry.in_use(),
                "ipc_space_destroy: ipc_right_clean, not yet"
            );
            panic!("ipc_space_destroy: ie_free, not yet");
        }
        // As rdxtree_remove_all: with the zero entry alone, neither tree has a
        // node.
        state.map = RadixTree::new();
        state.reverse_map = RadixTree::new();
        drop(state);
        // SAFETY: a space in use holds that reference, given up here, once, as
        // it is no longer in use.
        drop(unsafe { Reference::from_raw(NonNull::from(self)) });
    }

    /// The space of the task whose port its send right `name` is for,
    /// referenced, as `port_name_to_space` finds it; `None` if the port is
    /// dead or stands for no task.
    ///
    /// # Panics
    ///
    /// If `name` holds no send right alone: not yet. If its entry has no
    /// port.
    pub fn task_space(&self, name: PortName) -> Option<Reference<Self>> {
        // fast_send_right_lookup.
        let state = self.state.read();
        let entry = state
            .lookup(name)
            .filter(|entry| entry.bits.rights.bits() == Rights::SEND.bits())
            .unwrap_or_else(|| panic!("port_name_to_space: abort, not yet"));
        let Some(Object::Port(port)) = &entry.object else {
            panic!("port_name_to_space: a send right with no port");
        };
        // The port locked under the space's lock.
        let space = port.task_space();
        drop(state);
        space
    }
}

// SAFETY: its count is its own, and every space but the special ones, whose
// own reference never drops, is a box of `CACHE`, which `free` takes back.
unsafe impl Counted for Space {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `space`, as `ipc_space_release` does at zero: `is_free` gives
    /// it back to its cache.
    unsafe fn free(space: NonNull<Self>) {
        // SAFETY: a `CACHE` box whose first reference its task took from
        // the box's own borrow; nothing uses it after.
        drop(unsafe { CacheBox::from_raw(&CACHE, space) });
    }
}

impl SpaceState {
    /// The entry of `name`, if it holds rights, as `ipc_entry_lookup` finds
    /// it.
    pub(super) fn lookup(&self, name: PortName) -> Option<&Entry> {
        self.map.get(name.key()).filter(|entry| entry.in_use())
    }

    /// The last freed name, else the lowest free one, and its entry, which
    /// holds no rights.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the entry or a map node can't be allocated.
    ///
    /// # Panics
    ///
    /// If the space is dead: not yet. If a freed name has no entry, or its
    /// entry is not free.
    pub(super) fn allocate(&mut self) -> Result<(PortName, &mut Entry), Error> {
        assert!(self.active, "ipc_entry_alloc: a dead space, not yet");
        // As ipc_entry_get, with no generations.
        if let Some(name) = self.free_names.pop() {
            let entry = self
                .map
                .get_mut(name.key())
                .expect("ipc_entry_get: no free entry");
            assert!(
                !entry.in_use() && entry.bits.user_references == 0 && !entry.bits.message_accepted,
                "ipc_entry_get: a free entry with rights"
            );
            entry.request = 0;
            assert!(name.is_valid(), "ipc_entry_get: an invalid name");
            assert!(
                entry.object.is_none(),
                "ipc_entry_get: a free entry with a port"
            );
            self.size += 1;
            return Ok((name, entry));
        }
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
pub static REPLY_SPACE: Space = Space::special();
