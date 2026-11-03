// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Address maps.

use core::ops::Range;

use intrusive_collections::{
    Bound, KeyAdapter, RBTree, RBTreeAtomicLink, UnsafeRef, intrusive_adapter,
};

use super::{Inheritance, Protection};
use crate::arch::Pmap;
use crate::kernel::Error;
use crate::kernel::slab::{Cache, CacheBox};
use crate::sync::SleepLock;

/// An address map: ranges of virtual memory and what they map; so far only
/// what a new map sets.
#[expect(dead_code, reason = "pmap_enter and vm_map_reference read them")]
pub struct Map {
    /// The addresses it may map.
    bounds: Range<usize>,
    /// The physical map its mappings go into.
    pmap: &'static Pmap,
    /// How many references it has.
    references: u32,
    /// What changes as it maps and unmaps, behind its lock.
    contents: SleepLock<Contents>,
}

/// What a map's lock guards.
struct Contents {
    /// Its entries, by address.
    entries: RBTree<EntryAdapter>,
    /// How much it maps, in bytes.
    size: usize,
}

impl Map {
    /// An empty map of `bounds` over `pmap`, with one reference.
    pub const fn new(pmap: &'static Pmap, bounds: Range<usize>) -> Self {
        Self {
            bounds,
            pmap,
            references: 1,
            contents: SleepLock::new(Contents {
                entries: RBTree::new(EntryAdapter::NEW),
                size: 0,
            }),
        }
    }

    /// Maps `range` with nothing behind it yet, allowing `protection` now
    /// and at most `max_protection`, and giving child tasks `inheritance`.
    ///
    /// # Errors
    ///
    /// `InvalidAddress` if `range` is empty or not inside the map, `NoSpace`
    /// if it overlaps an entry, and `ResourceShortage` if no entry can be
    /// allocated.
    pub fn enter(
        &self,
        range: Range<usize>,
        protection: Protection,
        max_protection: Protection,
        inheritance: Inheritance,
    ) -> Result<(), Error> {
        let mut contents = self.contents.write();
        if range.start < self.bounds.start || range.end > self.bounds.end || range.is_empty() {
            return Err(Error::InvalidAddress);
        }
        let below = contents.entries.upper_bound(Bound::Included(&range.start));
        let above = contents.entries.lower_bound(Bound::Excluded(&range.start));
        if below
            .get()
            .is_some_and(|entry| entry.range.end > range.start)
            || above
                .get()
                .is_some_and(|entry| entry.range.start < range.end)
        {
            return Err(Error::NoSpace);
        }
        let size = range.end - range.start;
        let entry = ENTRY_CACHE.allocate(Entry {
            tree_link: RBTreeAtomicLink::new(),
            range,
            protection,
            max_protection,
            inheritance,
        })?;
        // SAFETY: the tree takes the entry for good.
        contents
            .entries
            .insert(unsafe { UnsafeRef::from_raw(CacheBox::into_raw(entry).as_ptr()) });
        contents.size += size;
        drop(contents);
        Ok(())
    }
}

/// A range of a map and what it maps; so far only what an entry with no
/// object sets.
pub struct Entry {
    /// Its place in its map's tree.
    tree_link: RBTreeAtomicLink,
    /// The addresses it covers.
    range: Range<usize>,
    /// What it allows now.
    #[expect(dead_code, reason = "vm_fault and vm_map_protect read it")]
    protection: Protection,
    /// The most it may ever allow.
    #[expect(dead_code, reason = "vm_map_protect reads it")]
    max_protection: Protection,
    /// What a child task gets of it.
    #[expect(dead_code, reason = "vm_map_fork reads it")]
    inheritance: Inheritance,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    EntryAdapter = UnsafeRef<Entry>: Entry { tree_link => RBTreeAtomicLink }
);

impl<'a> KeyAdapter<'a> for EntryAdapter {
    type Key = usize;

    fn get_key(&self, entry: &'a Entry) -> usize {
        entry.range.start
    }
}

/// Memory copied out of a map while in transit, such as out-of-line data; so
/// far only its kind and size.
#[expect(dead_code, reason = "vm_map_copyin and vm_map_copyout use them")]
pub struct MapCopy {
    /// How it holds the memory.
    kind: CopyKind,
    /// Its size, in bytes.
    size: usize,
}

/// How a copy holds its memory.
#[expect(dead_code, reason = "vm_map_copyin and vm_map_copyout use them")]
enum CopyKind {
    /// As map entries, like a map.
    Entries,
    /// As one object, mostly on the pageout path.
    Object,
    /// As a list of pages.
    Pages,
}

/// The cache maps come from.
static MAP_CACHE: Cache<Map> = Cache::new("vm_map");

/// The cache entries come from.
static ENTRY_CACHE: Cache<Entry> = Cache::new("vm_map_entry");

/// The cache copies come from.
static COPY_CACHE: Cache<MapCopy> = Cache::new("vm_map_copy");

/// Sets up address maps, on the boot CPU.
pub fn init() {
    MAP_CACHE.register();
    ENTRY_CACHE.register();
    COPY_CACHE.register();
}
