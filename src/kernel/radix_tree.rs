// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Radix trees of pointers under 64-bit keys, after GNU Mach's rdxtree;
//! so far only insertion. A level takes six bits of the key, and a tree is
//! only as tall as its largest key needs.

use core::marker::PhantomData;
use core::ptr::{self, NonNull};

use super::slab::{Cache, CacheBox};

/// The key bits a level takes.
const RADIX: u32 = 6;

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

impl Node {
    /// A new empty node `height` levels above the leaves.
    fn create(height: u16) -> Result<*mut Self, Error> {
        let node = NODE_CACHE
            .allocate(Self {
                parent: ptr::null_mut(),
                index: 0,
                height,
                count: 0,
                bitmap: u64::MAX,
                entries: [ptr::null_mut(); SIZE],
            })
            .map_err(|_| Error::ResourceShortage)?;
        Ok(CacheBox::into_raw(node).as_ptr())
    }
}

/// Why an insertion failed.
#[derive(Debug)]
pub enum Error {
    /// The key holds a pointer already.
    Occupied,
    /// No node could be allocated.
    ResourceShortage,
}

/// An entry that is a child node: the node's address with its low bit set.
fn child(node: *mut Node) -> *mut () {
    node.cast::<()>().map_addr(|address| address | 1)
}

/// The node an entry for a child points to.
fn node_of(entry: *mut ()) -> *mut Node {
    entry.map_addr(|address| address & !1).cast()
}

/// The largest key a tree of `height` levels holds.
const fn max_key(height: u16) -> u64 {
    let shift = height as u32 * RADIX;
    if shift < u64::BITS {
        (1 << shift) - 1
    } else {
        u64::MAX
    }
}

/// The key's digit at `level` levels above the leaves.
fn digit(key: u64, level: u16) -> usize {
    usize::try_from(key >> (u32::from(level) * RADIX) & 63).unwrap()
}

/// A radix tree of pointers to `T`, which it never reads; its nodes are
/// its own. A tree of height 0 holds key 0 alone, in its root.
pub struct RadixTree<T> {
    /// How many levels of nodes it has.
    height: u16,
    /// The pointer of key 0 at height 0, else the top node, or null.
    root: *mut (),
    marker: PhantomData<NonNull<T>>,
}

// SAFETY: a tree only holds pointers, which it never reads, and nodes only
// it reaches; whoever holds the tree serialises its use.
unsafe impl<T> Send for RadixTree<T> {}
// SAFETY: as above.
unsafe impl<T> Sync for RadixTree<T> {}

impl<T> RadixTree<T> {
    /// An empty one.
    pub const fn new() -> Self {
        Self {
            height: 0,
            root: ptr::null_mut(),
            marker: PhantomData,
        }
    }

    /// Stores `pointer`, two-byte aligned, under `key`. When a node cannot
    /// be allocated, those this call made stay, empty, until nodes can be
    /// freed.
    ///
    /// # Errors
    ///
    /// `Occupied` if `key` holds a pointer, and `ResourceShortage` if a
    /// node could not be allocated.
    pub fn insert(&mut self, key: u64, pointer: NonNull<T>) -> Result<(), Error> {
        if key > max_key(self.height) {
            self.grow(key)?;
        }
        if self.height == 0 {
            if !self.root.is_null() {
                return Err(Error::Occupied);
            }
            self.root = pointer.as_ptr().cast();
            return Ok(());
        }
        // SAFETY: the tree's nodes are live and only it reaches them, and
        // `&mut self` makes this the only use.
        unsafe {
            if self.root.is_null() {
                self.root = child(Node::create(self.height - 1)?);
            }
            let mut node = node_of(self.root);
            for level in (1..self.height).rev() {
                let index = digit(key, level);
                if (*node).entries[index].is_null() {
                    let next = Node::create(level - 1)?;
                    (*next).parent = node;
                    (*next).index = u16::try_from(index).unwrap();
                    (*node).entries[index] = child(next);
                    (*node).count += 1;
                }
                node = node_of((*node).entries[index]);
            }
            let index = digit(key, 0);
            if !(*node).entries[index].is_null() {
                return Err(Error::Occupied);
            }
            (*node).entries[index] = pointer.as_ptr().cast();
            (*node).count += 1;
            // The slot is taken; a full node leaves no room below its parent.
            let (mut node, mut index) = (node, index);
            loop {
                (*node).bitmap &= !(1 << index);
                if usize::from((*node).count) != SIZE || (*node).parent.is_null() {
                    break;
                }
                index = usize::from((*node).index);
                node = (*node).parent;
            }
        }
        Ok(())
    }

    /// Adds levels on top until `key` fits, the old top below the new one.
    fn grow(&mut self, key: u64) -> Result<(), Error> {
        let mut height = self.height + 1;
        while key > max_key(height) {
            height += 1;
        }
        if self.root.is_null() {
            self.height = height;
            return Ok(());
        }
        while self.height < height {
            let node = Node::create(self.height)?;
            // SAFETY: the new node is only this tree's, and so is its root.
            unsafe {
                if self.height == 0 {
                    // Key 0's pointer, which takes its slot.
                    (*node).bitmap &= !1;
                } else {
                    let top = node_of(self.root);
                    (*top).parent = node;
                    (*top).index = 0;
                    if (*top).bitmap == 0 {
                        (*node).bitmap &= !1;
                    }
                }
                (*node).entries[0] = self.root;
                (*node).count = 1;
            }
            self.height += 1;
            self.root = child(node);
        }
        Ok(())
    }
}
