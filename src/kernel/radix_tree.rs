// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Radix trees of pointers under 64-bit keys, after GNU Mach's rdxtree; so
//! far only the cache their nodes come from.

use super::slab::Cache;

/// How many entries a node holds: one per six-bit digit of a key.
const SIZE: usize = 64;

/// One level of a tree: an entry per digit, each a child node or a leaf.
#[repr(C)]
struct Node {
    /// Its parent; null at the root.
    parent: *mut Self,
    /// Its entry in its parent.
    index: u16,
    /// How many levels lie below it.
    height: u16,
    /// How many of its entries are occupied.
    count: u16,
    /// Its entries with room below them, a bit each, for key allocation.
    bitmap: u64,
    /// Its children and leaves; a child's entry has its low bit set.
    entries: [*mut (); SIZE],
}

/// The cache nodes come from.
static NODE_CACHE: Cache<Node> = Cache::new("rdxtree_node");

/// Sets up radix trees, on the boot CPU.
pub fn init() {
    NODE_CACHE.register();
}
