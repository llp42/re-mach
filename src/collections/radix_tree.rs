// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Radix trees of pointers under 64-bit keys, after GNU Mach's rdxtree;
//! so far insertion, under a key or the lowest free one, and lookup. A
//! level takes six bits of the key, and a tree is only as tall as its
//! largest key needs.

use core::marker::PhantomData;
use core::ptr::{self, NonNull};

use crate::slab::{Cache, CacheBox};

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

    /// Stores `pointer`, two-byte aligned, under `key`.
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
                // Back to an empty tree's height 0 if this fails.
                self.root = child(Node::create(self.height - 1).inspect_err(|_| self.height = 0)?);
            }
            let mut node = node_of(self.root);
            for level in (1..self.height).rev() {
                let index = digit(key, level);
                if (*node).entries[index].is_null() {
                    let next = Node::create(level - 1).inspect_err(|_| self.cleanup(node))?;
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

    /// Stores `pointer`, two-byte aligned, under the lowest free key, and
    /// gives that key.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if a node could not be allocated.
    pub fn insert_alloc(&mut self, pointer: NonNull<T>) -> Result<u64, Error> {
        if self.height == 0 {
            if self.root.is_null() {
                self.root = pointer.as_ptr().cast();
                return Ok(0);
            }
        } else {
            let mut key = 0;
            // SAFETY: the tree's nodes are live and only it reaches them, and
            // `&mut self` makes this the only use; a tree with levels has a
            // top node.
            unsafe {
                let mut node = node_of(self.root);
                for level in (0..self.height).rev() {
                    // Only the top node can lack room below.
                    if (*node).bitmap == 0 {
                        break;
                    }
                    let mut index = usize::try_from((*node).bitmap.trailing_zeros()).unwrap();
                    key |= u64::try_from(index).unwrap() << (u32::from(level) * RADIX);
                    if level == 0 {
                        (*node).entries[index] = pointer.as_ptr().cast();
                        (*node).count += 1;
                        // The slot is taken; a full node leaves no room below
                        // its parent.
                        loop {
                            (*node).bitmap &= !(1 << index);
                            if usize::from((*node).count) != SIZE || (*node).parent.is_null() {
                                break;
                            }
                            index = usize::from((*node).index);
                            node = (*node).parent;
                        }
                        return Ok(key);
                    }
                    if (*node).entries[index].is_null() {
                        let next = Node::create(level - 1).inspect_err(|_| self.cleanup(node))?;
                        (*next).parent = node;
                        (*next).index = u16::try_from(index).unwrap();
                        (*node).entries[index] = child(next);
                        (*node).count += 1;
                    }
                    node = node_of((*node).entries[index]);
                }
            }
        }
        // Every key the tree's height reaches is taken: the next one.
        let key = max_key(self.height) + 1;
        self.insert(key, pointer)?;
        Ok(key)
    }

    /// The pointer stored under `key`, if any.
    pub fn lookup(&self, key: u64) -> Option<NonNull<T>> {
        if key > max_key(self.height) {
            return None;
        }
        let mut entry = self.root;
        for level in (0..self.height).rev() {
            if entry.is_null() {
                return None;
            }
            // SAFETY: the tree's nodes are live and only it reaches them, and
            // `&self` keeps them so.
            entry = unsafe { (*node_of(entry)).entries[digit(key, level)] };
        }
        NonNull::new(entry.cast())
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
            let node = Node::create(self.height).inspect_err(|_| self.shrink())?;
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

    /// Frees the top nodes that hold their entry 0 alone, lowering the tree.
    fn shrink(&mut self) {
        // SAFETY: the tree's nodes are live and only it reaches them, and
        // `&mut self` makes this the only use.
        unsafe {
            while self.height > 0 {
                let node = node_of(self.root);
                let entry = (*node).entries[0];
                if (*node).count != 1 || entry.is_null() {
                    break;
                }
                self.height -= 1;
                if self.height > 0 {
                    (*node_of(entry)).parent = ptr::null_mut();
                }
                self.root = entry;
                drop(CacheBox::from_raw(&NODE_CACHE, NonNull::from(&mut *node)));
            }
        }
    }

    /// Frees the empty nodes from `node` up; reaching the top, lowers the
    /// tree as `shrink` does.
    ///
    /// # Safety
    ///
    /// `node` is one of the tree's nodes.
    unsafe fn cleanup(&mut self, mut node: *mut Node) {
        // SAFETY: the caller's; the tree's nodes are live and only it
        // reaches them, and `&mut self` makes this the only use.
        unsafe {
            loop {
                let parent = (*node).parent;
                if (*node).count != 0 {
                    if parent.is_null() {
                        self.shrink();
                    }
                    break;
                }
                if parent.is_null() {
                    self.height = 0;
                    self.root = ptr::null_mut();
                    drop(CacheBox::from_raw(&NODE_CACHE, NonNull::from(&mut *node)));
                    break;
                }
                (*parent).entries[usize::from((*node).index)] = ptr::null_mut();
                (*parent).count -= 1;
                drop(CacheBox::from_raw(&NODE_CACHE, NonNull::from(&mut *node)));
                node = parent;
            }
        }
    }
}
