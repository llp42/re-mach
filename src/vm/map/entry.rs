// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use intrusive_collections::rbtree::Cursor;
use intrusive_collections::{KeyAdapter, RBTree, RBTreeAtomicLink, UnsafeRef, intrusive_adapter};

use super::lookup::Lookup;
use super::map::Map;
use crate::reference::Reference;
use crate::slab::{Cache, CacheBox};
use crate::sync::TokenGuard;
use crate::vm::object::{object::Object, offset::Offset, state::ObjectState};
use crate::vm::{Inheritance, Protection, VirtualAddress};

/// Entries allocated before a map's lock, for one operation to take under
/// it, so none is allocated there; those left go back to their cache when
/// it drops.
pub(super) struct EntryReservation {
    /// Its entries, through their tree links, which an entry in no map does
    /// not use.
    pub(super) entries: RBTree<EntryAdapter>,
    /// How many it holds.
    pub(super) count: usize,
}

impl EntryReservation {
    /// One holding four entries.
    ///
    /// # Panics
    ///
    /// As `reserve`.
    pub(super) fn new() -> Self {
        let mut reservation = Self {
            entries: RBTree::new(EntryAdapter::NEW),
            count: 0,
        };
        reservation.reserve(4);
        reservation
    }

    /// Allocates entries until it holds `count`.
    ///
    /// # Panics
    ///
    /// If an entry can't be allocated.
    pub(super) fn reserve(&mut self, count: usize) {
        while self.count < count {
            let entry = ENTRY_CACHE
                .allocate(Entry {
                    tree_link: RBTreeAtomicLink::new(),
                    gap_link: RBTreeAtomicLink::new(),
                    start: AtomicUsize::new(0),
                    end: AtomicUsize::new(0),
                    gap: AtomicUsize::new(0),
                    gap_sequence: AtomicU64::new(0),
                    protection: AtomicU32::new(Protection::empty().bits()),
                    max_protection: AtomicU32::new(Protection::empty().bits()),
                    inheritance: Inheritance::Copy,
                    backing: Backing::Object {
                        object: AtomicPtr::new(ptr::null_mut()),
                        offset: AtomicUsize::new(0),
                    },
                    wire_count: AtomicU32::new(0),
                    wired_access: AtomicU32::new(Protection::empty().bits()),
                    in_transition: AtomicBool::new(false),
                    needs_copy: AtomicBool::new(false),
                    kernel_stack: false,
                })
                .unwrap_or_else(|error| panic!("vm_map_entry_create: {error:?}"));
            let entry = CacheBox::into_raw(entry).as_ptr();
            // SAFETY: the tree takes the entry, once, until `take` gives it
            // back.
            self.entries.insert(unsafe { UnsafeRef::from_raw(entry) });
            self.count += 1;
        }
    }

    /// One of its entries, to be filled in.
    ///
    /// # Panics
    ///
    /// If none is left: more entries than reserved, not yet.
    pub(super) fn take(&mut self) -> CacheBox<Entry> {
        let entry = self
            .entries
            .front_mut()
            .remove()
            .expect("vm_map: more entries than reserved, not yet");
        self.count -= 1;
        let entry = NonNull::new(UnsafeRef::into_raw(entry)).unwrap();
        // SAFETY: its entries come from `CacheBox::into_raw` of
        // `ENTRY_CACHE`'s boxes (`reserve`), and its tree held the only
        // pointer to each.
        unsafe { CacheBox::from_raw(&ENTRY_CACHE, entry) }
    }
}

impl Drop for EntryReservation {
    /// Gives the entries left back to their cache.
    fn drop(&mut self) {
        while self.count != 0 {
            drop(self.take());
        }
    }
}

/// A range of a map and what it maps; so far only what an entry of an
/// object, wired or not, or of a submap sets.
pub struct Entry {
    /// Its place in its map's tree.
    pub(super) tree_link: RBTreeAtomicLink,
    /// Its place in its map's gap tree, while free space follows it.
    pub(super) gap_link: RBTreeAtomicLink,
    /// Where it starts, as a `VirtualAddress`; changed only under the map's
    /// write lock.
    pub(super) start: AtomicUsize,
    /// Where it ends, past its last address, as a `VirtualAddress`; changed
    /// only under the map's write lock.
    pub(super) end: AtomicUsize,
    /// The free space after it, up to the next entry or the map's end;
    /// changed only under the map's write lock.
    pub(super) gap: AtomicUsize,
    /// Its place among equal gaps: its map's `gap_inserts` when its gap was
    /// last inserted; changed only under the map's write lock.
    pub(super) gap_sequence: AtomicU64,
    /// What it allows now, as `Protection`'s bits; changed only under the
    /// map's write lock.
    pub(super) protection: AtomicU32,
    /// The most it may ever allow, as `Protection`'s bits; changed only
    /// under the map's write lock.
    pub(super) max_protection: AtomicU32,
    /// What a child task gets of it.
    pub(super) inheritance: Inheritance,
    /// What backs it.
    pub(in crate::vm) backing: Backing,
    /// How many times it is wired: its pages stay resident and mapped;
    /// changed only under the map's write lock.
    pub(super) wire_count: AtomicU32,
    /// What it allows while wired, as `Protection`'s bits; changed only
    /// under the map's write lock.
    pub(super) wired_access: AtomicU32,
    /// Whether a wiring has it while the map is unlocked, which keeps it
    /// from merging or going; changed only under the map's write lock.
    pub(super) in_transition: AtomicBool,
    /// Whether a copy shares its object, which is then shadowed before it
    /// is written; changed only under the map's write lock.
    pub(super) needs_copy: AtomicBool,
    /// Whether it is a kernel stack, whose first page, its guard, is never
    /// wired; GNU Mach's stacks have no guard.
    pub(in crate::vm) kernel_stack: bool,
}

impl Entry {
    /// Where it starts.
    pub(in crate::vm) fn start(&self) -> VirtualAddress {
        VirtualAddress::from(self.start.load(Ordering::Relaxed))
    }

    /// Where it ends, past its last address.
    pub(in crate::vm) fn end(&self) -> VirtualAddress {
        VirtualAddress::from(self.end.load(Ordering::Relaxed))
    }

    /// How many times it is wired.
    pub(super) fn wire_count(&self) -> u32 {
        self.wire_count.load(Ordering::Relaxed)
    }

    /// What it allows while wired.
    pub(super) fn wired_access(&self) -> Protection {
        Protection::from_bits_retain(self.wired_access.load(Ordering::Relaxed))
    }

    /// What it allows now.
    pub(in crate::vm) fn protection(&self) -> Protection {
        Protection::from_bits_retain(self.protection.load(Ordering::Relaxed))
    }

    /// The most it may ever allow.
    pub(super) fn max_protection(&self) -> Protection {
        Protection::from_bits_retain(self.max_protection.load(Ordering::Relaxed))
    }

    /// Its object, from an offset, once `vm_map_lookup`'s checks find a fault
    /// asking for `access` may be resolved in it.
    ///
    /// # Panics
    ///
    /// If it is a submap or doesn't allow `access`: not yet.
    pub(super) fn checked_backing(&self, access: Protection) -> (&AtomicPtr<Object>, &AtomicUsize) {
        let Backing::Object { object, offset } = &self.backing else {
            panic!("vm_map_lookup: a submap, not yet");
        };
        assert!(
            self.protection().contains(access),
            "vm_map_lookup: KERN_PROTECTION_FAILURE, not yet"
        );
        (object, offset)
    }

    /// What a lookup of `address` in it finds, with `object` behind it from
    /// `offset`, with a reference, its `state` locked, at the map's
    /// `version`.
    pub(super) fn found(
        &self,
        address: VirtualAddress,
        object: Reference<Object>,
        state: TokenGuard<'static, ObjectState>,
        offset: Offset,
        version: u32,
    ) -> Lookup {
        // Reading what a copy shares: writing would need a shadow first.
        let mut protection = self.protection();
        if self.needs_copy.load(Ordering::Relaxed) {
            protection -= Protection::WRITE;
        }
        Lookup {
            state,
            object,
            offset: offset + (address - self.start()),
            protection,
            wired: self.wire_count() != 0,
            version,
        }
    }

    /// A copy of it from `reservation`, as `vm_map_entry_copy` makes one:
    /// unwired, out of transition, no kernel stack, and needing a copy if it
    /// does; its object takes no reference for it.
    ///
    /// # Panics
    ///
    /// For a submap: not yet. As `EntryReservation::take`.
    pub(super) fn copy(&self, reservation: &mut EntryReservation) -> CacheBox<Self> {
        let Backing::Object { object, offset } = &self.backing else {
            panic!("vm_map_entry_copy: a submap, not yet");
        };
        let mut new = reservation.take();
        *new = Self {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            start: AtomicUsize::new(usize::from(self.start())),
            end: AtomicUsize::new(usize::from(self.end())),
            gap: AtomicUsize::new(self.gap.load(Ordering::Relaxed)),
            gap_sequence: AtomicU64::new(0),
            protection: AtomicU32::new(self.protection().bits()),
            max_protection: AtomicU32::new(self.max_protection().bits()),
            inheritance: self.inheritance,
            backing: Backing::Object {
                object: AtomicPtr::new(object.load(Ordering::Relaxed)),
                offset: AtomicUsize::new(offset.load(Ordering::Relaxed)),
            },
            wire_count: AtomicU32::new(0),
            wired_access: AtomicU32::new(Protection::empty().bits()),
            in_transition: AtomicBool::new(false),
            needs_copy: AtomicBool::new(self.needs_copy.load(Ordering::Relaxed)),
            kernel_stack: false,
        };
        new
    }
}

/// Merges the entry at `cursor` into the one before it, if they meet and map
/// alike.
///
/// # Panics
///
/// If they would merge: not yet.
pub(super) fn coalesce(cursor: &Cursor<'_, EntryAdapter>) {
    let (Some(entry), Some(previous)) = (cursor.get(), cursor.peek_prev().get()) else {
        return;
    };
    if previous.end() != entry.start()
        || matches!(previous.backing, Backing::Submap(_))
        || matches!(entry.backing, Backing::Submap(_))
        || previous.in_transition.load(Ordering::Relaxed)
        || entry.in_transition.load(Ordering::Relaxed)
        || previous.inheritance != entry.inheritance
        || previous.protection() != entry.protection()
        || previous.max_protection() != entry.max_protection()
        || previous.needs_copy.load(Ordering::Relaxed) != entry.needs_copy.load(Ordering::Relaxed)
        || previous.wire_count() != entry.wire_count()
        || previous.kernel_stack != entry.kernel_stack
    {
        return;
    }
    panic!("vm_map_coalesce_entry: vm_object_coalesce, not yet");
}

/// What backs an entry; only the kinds used here.
pub enum Backing {
    /// A map of its own over the same physical map, such as the kernel's
    /// IPC buffers.
    Submap(#[expect(dead_code, reason = "vm_map_lookup descends into it")] &'static Map),
    /// An object from `offset` on, whose reference it holds as
    /// `Reference::into_raw` gave it up; null until it has one. Both change
    /// only under the map's write lock.
    Object {
        object: AtomicPtr<Object>,
        offset: AtomicUsize,
    },
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub(super) EntryAdapter = UnsafeRef<Entry>: Entry { tree_link => RBTreeAtomicLink }
);

impl<'a> KeyAdapter<'a> for EntryAdapter {
    type Key = VirtualAddress;

    fn get_key(&self, entry: &'a Entry) -> VirtualAddress {
        entry.start()
    }
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub(super) GapAdapter = UnsafeRef<Entry>: Entry { gap_link => RBTreeAtomicLink }
);

impl<'a> KeyAdapter<'a> for GapAdapter {
    type Key = (usize, u64);

    fn get_key(&self, entry: &'a Entry) -> (usize, u64) {
        (
            entry.gap.load(Ordering::Relaxed),
            entry.gap_sequence.load(Ordering::Relaxed),
        )
    }
}

/// The cache entries come from.
pub(super) static ENTRY_CACHE: Cache<Entry> = Cache::new("vm_map_entry");
