// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Radix trees of pointers under 64-bit keys, after GNU Mach's rdxtree;
//! so far insertion, under a key or the lowest free one, and lookup. A
//! level takes six bits of the key, and a tree is only as tall as its
//! largest key needs. A tree of owners lends what they point to.

use core::marker::PhantomData;
use core::ptr::NonNull;

use crate::slab::{Cache, CacheBox};

/// The key bits a level takes.
const RADIX: u32 = 6;

/// How many entries a node holds: one per six-bit digit of a key.
const SIZE: usize = 64;

/// A pointer a tree can hold.
pub trait Pointer {
    /// What it points to.
    type Target;

    /// Gives it up to a tree, as its target's address.
    fn into_raw(self) -> NonNull<Self::Target>;
}

impl<T> Pointer for NonNull<T> {
    type Target = T;

    fn into_raw(self) -> Self {
        self
    }
}

impl<T> Pointer for CacheBox<T> {
    type Target = T;

    fn into_raw(self) -> NonNull<T> {
        Self::into_raw(self)
    }
}

/// A pointer that owns its target, which a tree holding it lends: shared
/// through `&self`, exclusively through `&mut self`.
///
/// # Safety
///
/// `into_raw` gives a live target that nothing else points to and that is
/// never `SHARED`; the target may move between threads with the tree.
/// `SHARED` points to a `static` that is never written.
pub unsafe trait Owner: Pointer<Target: 'static> {
    /// A target trees may hold besides their own, lent only shared.
    const SHARED: &'static Self::Target;
}

/// One level of a tree: an entry per digit, each a child node or a leaf.
struct Node {
    /// Its parent; none at the root.
    parent: Option<NonNull<Self>>,
    /// Its entry in its parent.
    index: u16,
    /// How many levels lie below it.
    #[expect(dead_code, reason = "rdxtree_walk reads it")]
    height: u16,
    /// How many of its entries are occupied.
    count: u16,
    /// Its entries with room below them, a bit each, for key allocation.
    bitmap: u64,
    /// Its children and leaves.
    entries: [Slot; SIZE],
}

/// The cache nodes come from.
static NODE_CACHE: Cache<Node> = Cache::new("rdxtree_node");

/// Sets up radix trees, on the boot CPU.
pub fn init() {
    NODE_CACHE.register();
}

impl Node {
    /// A new empty node `height` levels above the leaves.
    fn create(height: u16) -> Result<NonNull<Self>, Error> {
        let node = NODE_CACHE
            .allocate(Self {
                parent: None,
                index: 0,
                height,
                count: 0,
                bitmap: u64::MAX,
                entries: [Slot::EMPTY; SIZE],
            })
            .map_err(|_| Error::ResourceShortage)?;
        Ok(CacheBox::into_raw(node))
    }
}

/// A node's entry, or a tree's root: empty, a child node or a leaf. A
/// child's address has its low bit set, but the level, not the bit, tells
/// a child from a leaf.
#[derive(Clone, Copy)]
struct Slot(Option<NonNull<()>>);

impl Slot {
    /// An empty one.
    const EMPTY: Self = Self(None);

    /// One holding the child `node`.
    fn child(node: NonNull<Node>) -> Self {
        Self(Some(node.cast().map_addr(|address| address | 1)))
    }

    /// One holding the leaf `pointer`.
    const fn leaf<T>(pointer: NonNull<T>) -> Self {
        Self(Some(pointer.cast()))
    }

    /// Whether it is empty.
    const fn is_empty(self) -> bool {
        self.0.is_none()
    }

    /// The child node it holds, if not empty; only above the leaves.
    fn node(self) -> Option<NonNull<Node>> {
        self.0
            .and_then(|entry| NonNull::new(entry.as_ptr().map_addr(|address| address & !1).cast()))
    }

    /// The leaf it holds, if not empty; only at the leaves.
    fn pointer<T>(self) -> Option<NonNull<T>> {
        self.0.map(NonNull::cast)
    }
}

/// Where a new pointer goes.
#[derive(Clone, Copy)]
enum Place {
    /// The root, at height 0.
    Root,
    /// An entry, by index, of a node just above the leaves.
    Leaf(NonNull<Node>, usize),
}

/// Why an insertion failed.
#[derive(Debug)]
pub enum Error {
    /// The key holds a pointer already.
    Occupied,
    /// No node could be allocated.
    ResourceShortage,
}

/// The largest key a tree of `height` levels holds.
fn max_key(height: u16) -> u64 {
    let shift = u32::from(height) * RADIX;
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

/// The lowest entry `bitmap` marks with room below, if any.
fn free_index(bitmap: u64) -> Option<usize> {
    (bitmap != 0).then(|| usize::try_from(bitmap.trailing_zeros()).unwrap())
}

/// A radix tree of `P` pointers, whose nodes are its own. A tree of height
/// 0 holds key 0 alone, in its root.
pub struct RadixTree<P> {
    /// How many levels of nodes it has.
    height: u16,
    /// The pointer of key 0 at height 0, else the top node, or empty.
    root: Slot,
    marker: PhantomData<P>,
}

// SAFETY: a tree's nodes are only its own, and whoever holds the tree
// serialises its use, its lends included; it follows targets only for
// owners, which `Owner` lets move between threads.
unsafe impl<P> Send for RadixTree<P> {}
// SAFETY: as above; it lends its targets shared only where they are `Sync`.
unsafe impl<P: Pointer<Target: Sync>> Sync for RadixTree<P> {}

impl<P: Pointer> RadixTree<P> {
    /// An empty one.
    pub const fn new() -> Self {
        Self {
            height: 0,
            root: Slot::EMPTY,
            marker: PhantomData,
        }
    }

    /// Where the pointer stored under `key` points, if any. An owner's target
    /// may be read through it only while no exclusive lend is alive.
    pub fn lookup(&self, key: u64) -> Option<NonNull<P::Target>> {
        if key > max_key(self.height) {
            return None;
        }
        let mut slot = self.root;
        for level in (0..self.height).rev() {
            slot = self.node(slot.node()?).entries[digit(key, level)];
        }
        slot.pointer()
    }

    /// The node `node`, one of the tree's, read from its slots or just made.
    #[expect(clippy::unused_self, reason = "the tree's borrow lends its nodes")]
    const fn node(&self, node: NonNull<Node>) -> &Node {
        // SAFETY: the tree's nodes are live and only it reaches them, and
        // `&self` keeps them so.
        unsafe { node.as_ref() }
    }

    /// As `node`, exclusively.
    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "the tree's borrow lends its nodes"
    )]
    const fn node_mut(&mut self, mut node: NonNull<Node>) -> &mut Node {
        // SAFETY: as in `node`; `&mut self` makes this the only use.
        unsafe { node.as_mut() }
    }

    /// Frees `node`, one of the tree's that it no longer reaches.
    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "the tree's borrow lends its nodes"
    )]
    fn free(&mut self, node: NonNull<Node>) {
        // SAFETY: it came from `into_raw` of a node box, and no slot reaches
        // it any more.
        drop(unsafe { CacheBox::from_raw(&NODE_CACHE, node) });
    }

    /// Makes room for a pointer under `key`: gives where it goes.
    ///
    /// # Errors
    ///
    /// `Occupied` if `key` holds a pointer, and `ResourceShortage` if a
    /// node could not be allocated.
    fn place(&mut self, key: u64) -> Result<Place, Error> {
        if key > max_key(self.height) {
            self.grow(key)?;
        }
        if self.height == 0 {
            return if self.root.is_empty() {
                Ok(Place::Root)
            } else {
                Err(Error::Occupied)
            };
        }
        let mut node = if let Some(node) = self.root.node() {
            node
        } else {
            // Back to an empty tree's height 0 if this fails.
            let node = Node::create(self.height - 1).inspect_err(|_| self.height = 0)?;
            self.root = Slot::child(node);
            node
        };
        for level in (1..self.height).rev() {
            node = self.child(node, digit(key, level), level)?;
        }
        let index = digit(key, 0);
        if self.node(node).entries[index].is_empty() {
            Ok(Place::Leaf(node, index))
        } else {
            Err(Error::Occupied)
        }
    }

    /// Makes room for a pointer under the lowest free key the tree's height
    /// reaches: gives the key and where it goes, or none if every one is
    /// taken.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if a node could not be allocated.
    fn place_alloc(&mut self) -> Result<Option<(u64, Place)>, Error> {
        if self.height == 0 {
            return Ok(self.root.is_empty().then_some((0, Place::Root)));
        }
        // A tree with levels has a top node.
        let Some(mut node) = self.root.node() else {
            return Ok(None);
        };
        let mut key = 0;
        for level in (1..self.height).rev() {
            // Only the top node can lack room below.
            let Some(index) = free_index(self.node(node).bitmap) else {
                return Ok(None);
            };
            key |= u64::try_from(index).unwrap() << (u32::from(level) * RADIX);
            node = self.child(node, index, level)?;
        }
        Ok(free_index(self.node(node).bitmap).map(|index| {
            (
                key | u64::try_from(index).unwrap(),
                Place::Leaf(node, index),
            )
        }))
    }

    /// The child at `index` of `node`, which is `level` levels above the
    /// leaves, made if missing.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if it could not be allocated; the empty nodes from
    /// `node` up are freed.
    fn child(
        &mut self,
        node: NonNull<Node>,
        index: usize,
        level: u16,
    ) -> Result<NonNull<Node>, Error> {
        if let Some(child) = self.node(node).entries[index].node() {
            return Ok(child);
        }
        let child = Node::create(level - 1).inspect_err(|_| self.cleanup(node))?;
        let new = self.node_mut(child);
        new.parent = Some(node);
        new.index = u16::try_from(index).unwrap();
        let parent = self.node_mut(node);
        parent.entries[index] = Slot::child(child);
        parent.count += 1;
        Ok(child)
    }

    /// Stores `target` at `place`, which `place` or `place_alloc` gave.
    fn fill(&mut self, place: Place, target: NonNull<P::Target>) {
        match place {
            Place::Root => self.root = Slot::leaf(target),
            Place::Leaf(mut node, mut index) => {
                let leaf = self.node_mut(node);
                leaf.entries[index] = Slot::leaf(target);
                leaf.count += 1;
                // The slot is taken; a full node leaves no room below its
                // parent.
                loop {
                    let current = self.node_mut(node);
                    current.bitmap &= !(1 << index);
                    if usize::from(current.count) != SIZE {
                        break;
                    }
                    let Some(parent) = current.parent else {
                        break;
                    };
                    index = usize::from(current.index);
                    node = parent;
                }
            }
        }
    }

    /// Adds levels on top until `key` fits, the old top below the new one.
    fn grow(&mut self, key: u64) -> Result<(), Error> {
        let mut height = self.height + 1;
        while key > max_key(height) {
            height += 1;
        }
        if self.root.is_empty() {
            self.height = height;
            return Ok(());
        }
        while self.height < height {
            let node = Node::create(self.height).inspect_err(|_| self.shrink())?;
            let root = self.root;
            // Key 0's pointer takes its slot, and so does a full old top.
            let full = if self.height > 0
                && let Some(top) = root.node()
            {
                let top = self.node_mut(top);
                top.parent = Some(node);
                top.index = 0;
                top.bitmap == 0
            } else {
                true
            };
            let new = self.node_mut(node);
            if full {
                new.bitmap &= !1;
            }
            new.entries[0] = root;
            new.count = 1;
            self.height += 1;
            self.root = Slot::child(node);
        }
        Ok(())
    }

    /// Frees the top nodes that hold their entry 0 alone, lowering the tree.
    fn shrink(&mut self) {
        while self.height > 0 {
            let Some(node) = self.root.node() else {
                break;
            };
            let top = self.node(node);
            let entry = top.entries[0];
            if top.count != 1 || entry.is_empty() {
                break;
            }
            self.height -= 1;
            if self.height > 0
                && let Some(child) = entry.node()
            {
                self.node_mut(child).parent = None;
            }
            self.root = entry;
            self.free(node);
        }
    }

    /// Frees the empty nodes from `node`, one of the tree's, up; reaching
    /// the top, lowers the tree as `shrink` does.
    fn cleanup(&mut self, mut node: NonNull<Node>) {
        loop {
            let current = self.node(node);
            let (parent, index) = (current.parent, current.index);
            if current.count != 0 {
                if parent.is_none() {
                    self.shrink();
                }
                break;
            }
            let Some(parent) = parent else {
                self.height = 0;
                self.root = Slot::EMPTY;
                self.free(node);
                break;
            };
            let above = self.node_mut(parent);
            above.entries[usize::from(index)] = Slot::EMPTY;
            above.count -= 1;
            self.free(node);
            node = parent;
        }
    }
}

impl<T> RadixTree<NonNull<T>> {
    /// Stores `pointer` under `key`.
    ///
    /// # Errors
    ///
    /// `Occupied` if `key` holds a pointer, and `ResourceShortage` if a
    /// node could not be allocated.
    pub fn insert(&mut self, key: u64, pointer: NonNull<T>) -> Result<(), Error> {
        let place = self.place(key)?;
        self.fill(place, pointer);
        Ok(())
    }
}

impl<P: Owner> RadixTree<P> {
    /// Stores `SHARED` under `key`.
    ///
    /// # Errors
    ///
    /// As `insert`.
    pub fn insert_shared(&mut self, key: u64) -> Result<(), Error> {
        let place = self.place(key)?;
        self.fill(place, NonNull::from(P::SHARED));
        Ok(())
    }

    /// Stores `pointer` under `key`, and lends its target.
    ///
    /// # Errors
    ///
    /// `Occupied` if `key` holds a pointer, and `ResourceShortage` if a
    /// node could not be allocated; `pointer` is dropped.
    pub fn insert(&mut self, key: u64, pointer: P) -> Result<&mut P::Target, Error> {
        let place = self.place(key)?;
        let target = pointer.into_raw();
        self.fill(place, target);
        Ok(self.lend(target))
    }

    /// Stores `pointer` under the lowest free key, and gives that key and
    /// lends its target.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if a node could not be allocated; `pointer` is
    /// dropped.
    pub fn insert_alloc(&mut self, pointer: P) -> Result<(u64, &mut P::Target), Error> {
        let (key, place) = if let Some(found) = self.place_alloc()? {
            found
        } else {
            // Every key the tree's height reaches is taken: the next one.
            let key = max_key(self.height) + 1;
            (key, self.place(key)?)
        };
        let target = pointer.into_raw();
        self.fill(place, target);
        Ok((key, self.lend(target)))
    }

    /// The target stored under `key`, if any.
    pub fn get(&self, key: u64) -> Option<&P::Target> {
        // SAFETY: the tree owns the target, and `&self` keeps any exclusive
        // lend from being alive.
        self.lookup(key).map(|target| unsafe { target.as_ref() })
    }

    /// The target stored under `key`, if any and not `SHARED`.
    pub fn get_mut(&mut self, key: u64) -> Option<&mut P::Target> {
        let target = self.lookup(key)?;
        (target != NonNull::from(P::SHARED)).then(|| self.lend(target))
    }

    /// Lends `target`, one of the tree's that is not `SHARED`, exclusively.
    #[expect(
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "the tree's borrow lends its targets"
    )]
    const fn lend(&mut self, mut target: NonNull<P::Target>) -> &mut P::Target {
        // SAFETY: `target` is stored in this tree and is not `SHARED`, so the
        // tree owns it, and `&mut self` makes this the only lend.
        unsafe { target.as_mut() }
    }
}
