// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Address maps.

use core::ops::Range;

use intrusive_collections::{RBTree, RBTreeAtomicLink, UnsafeRef, intrusive_adapter};

use crate::kernel::slab::Cache;

/// An address map: ranges of virtual memory and what they map; so far only
/// its entries and bounds.
#[expect(dead_code, reason = "vm_map_setup and vm_map_enter use them")]
pub struct Map {
    /// Its entries, by address.
    entries: RBTree<EntryAdapter>,
    /// The addresses it may map.
    bounds: Range<usize>,
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

/// The cache maps come from.
static MAP_CACHE: Cache<Map> = Cache::new("vm_map");

/// The cache entries come from.
static ENTRY_CACHE: Cache<Entry> = Cache::new("vm_map_entry");

/// Sets up address maps, on the boot CPU.
pub fn init() {
    MAP_CACHE.register();
    ENTRY_CACHE.register();
}
