// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Address maps.

use core::ops::Range;

use intrusive_collections::{RBTree, RBTreeAtomicLink, UnsafeRef, intrusive_adapter};

use crate::arch::Pmap;
use crate::kernel::slab::Cache;
use crate::sync::SleepLock;

/// An address map: ranges of virtual memory and what they map; so far only
/// what a new map sets.
#[expect(
    dead_code,
    reason = "vm_map_enter, pmap_enter and vm_map_reference read them"
)]
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
#[expect(dead_code, reason = "vm_map_enter reads them")]
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
}

/// A range of a map and what it maps; so far only the range and its place in
/// its map's tree.
pub struct Entry {
    /// Its place in its map's tree.
    tree_link: RBTreeAtomicLink,
    /// The addresses it covers.
    #[expect(
        dead_code,
        reason = "vm_map_enter sets it and vm_map_lookup_entry reads it"
    )]
    range: Range<usize>,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    EntryAdapter = UnsafeRef<Entry>: Entry { tree_link => RBTreeAtomicLink }
);

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
