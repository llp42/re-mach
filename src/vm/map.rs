// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Address maps.

use core::ops::{DerefMut, Range};
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

use intrusive_collections::{
    Bound, KeyAdapter, RBTree, RBTreeAtomicLink, UnsafeRef, intrusive_adapter,
};

use super::object::{self, Object, ObjectState};
use super::page;
use super::pmap::MachinePmap as _;
use super::{Inheritance, Protection};
use crate::arch::{PAGE_SIZE, Pmap};
use crate::error::Error;
use crate::slab::{Cache, CacheBox};
use crate::sync::{OnceLock, SleepLock, TokenGuard};

/// An address map: ranges of virtual memory and what they map; so far only
/// what a new map sets.
#[expect(dead_code, reason = "vm_map_reference and vm_set_size_limit read them")]
pub struct Map {
    /// The addresses it may map.
    bounds: Range<usize>,
    /// The physical map its mappings go into.
    pmap: &'static Pmap,
    /// How many references it has.
    references: u32,
    /// The most it may map, in bytes.
    size_limit: usize,
    /// The most `size_limit` may be raised to.
    max_size_limit: usize,
    /// What changes as it maps and unmaps, behind its lock.
    contents: SleepLock<Contents>,
}

/// What a map's lock guards.
struct Contents {
    /// Its entries, by address.
    entries: RBTree<EntryAdapter>,
    /// The entries free space follows, by its size, then by address.
    gaps: RBTree<GapAdapter>,
    /// The free space before the first entry; all of the map while it is
    /// empty.
    leading_gap: usize,
    /// How much it maps, in bytes.
    size: usize,
    /// Its version, moved on by every write lock.
    timestamp: u32,
}

impl Map {
    /// An empty map of `bounds` over `pmap`, with one reference and no
    /// size limit.
    pub const fn new(pmap: &'static Pmap, bounds: Range<usize>) -> Self {
        let leading_gap = bounds.end - bounds.start;
        Self {
            bounds,
            pmap,
            references: 1,
            size_limit: usize::MAX,
            max_size_limit: usize::MAX,
            contents: SleepLock::new(Contents {
                entries: RBTree::new(EntryAdapter::NEW),
                gaps: RBTree::new(GapAdapter::NEW),
                leading_gap,
                size: 0,
                timestamp: 0,
            }),
        }
    }

    /// The physical map its mappings go into.
    pub const fn pmap(&self) -> &'static Pmap {
        self.pmap
    }

    /// A new empty map of `bounds` over `pmap`, a user task's, with one
    /// reference, its size limited to the memory's.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if it can't be allocated.
    pub fn create(pmap: &'static Pmap, bounds: Range<usize>) -> Result<CacheBox<Self>, Error> {
        let size = page::memory_size();
        MAP_CACHE.allocate(Self {
            size_limit: size,
            max_size_limit: size,
            ..Self::new(pmap, bounds)
        })
    }

    /// Maps `range` with nothing behind it yet, allowing `protection` now
    /// and at most `max_protection`, and giving child tasks `inheritance`.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if `range` is empty, `InvalidAddress` if it is not
    /// inside the map, `NoSpace` if it overlaps an entry or takes the map
    /// past its size limit, and `ResourceShortage` if no entry can be
    /// allocated.
    ///
    /// # Panics
    ///
    /// If `max_protection` is none, or a neighbouring entry could be
    /// extended over `range`: not yet.
    pub fn enter(
        &self,
        range: Range<usize>,
        protection: Protection,
        max_protection: Protection,
        inheritance: Inheritance,
    ) -> Result<(), Error> {
        if range.is_empty() {
            return Err(Error::InvalidArgument);
        }
        // Allocated before the lock; a failed check frees it after.
        let entry = ENTRY_CACHE.allocate(Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            range: range.clone(),
            gap: AtomicUsize::new(0),
            protection,
            max_protection,
            inheritance,
            backing: Backing::Object {
                object: AtomicPtr::new(ptr::null_mut()),
                offset: AtomicUsize::new(0),
            },
            wire_count: 0,
            wired_access: Protection::empty(),
        })?;
        let mut contents = self.lock();
        if range.start < self.bounds.start || range.end > self.bounds.end {
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
        assert!(
            !entry.max_protection.is_empty(),
            "vm_map_enter: a VM_PROT_NONE maximum, not yet"
        );
        // vm_map_enforce_limit, which the kernel's maps always pass.
        let size = contents
            .size
            .checked_add(range.end - range.start)
            .ok_or(Error::InvalidArgument)?;
        if size > self.size_limit {
            return Err(Error::NoSpace);
        }
        let extends = |neighbour: &Entry| {
            !matches!(neighbour.backing, Backing::Submap(_))
                && neighbour.inheritance == entry.inheritance
                && neighbour.protection == entry.protection
                && neighbour.max_protection == entry.max_protection
                && neighbour.wire_count == 0
        };
        assert!(
            !below
                .get()
                .is_some_and(|previous| previous.range.end == range.start && extends(previous)),
            "vm_map_enter: vm_object_coalesce with the previous entry, not yet"
        );
        assert!(
            !above
                .get()
                .is_some_and(|next| next.range.start == range.end && extends(next)),
            "vm_map_enter: vm_object_coalesce with the next entry, not yet"
        );
        let previous = below.clone_pointer();
        let next = above
            .get()
            .map_or(self.bounds.end, |entry| entry.range.start);
        entry.gap.store(next - range.end, Ordering::Relaxed);
        contents.link(self.bounds.start, previous, entry);
        drop(contents);
        Ok(())
    }

    /// Maps a new submap of `size` bytes wherever it fits, over this map's
    /// physical map, and sets `submap` to it.
    ///
    /// # Errors
    ///
    /// `NoSpace` if no free space fits it, and `ResourceShortage` if no
    /// entry can be allocated, which leaves `submap` set.
    ///
    /// # Panics
    ///
    /// If `submap` is set already.
    pub fn enter_submap(&self, size: usize, submap: &'static OnceLock<Self>) -> Result<(), Error> {
        let mut contents = self.lock();
        let (previous, start) = contents.find_space(self.bounds.start, size, PAGE_SIZE)?;
        let next = previous
            .as_ref()
            .map_or(self.bounds.start + contents.leading_gap, |entry| {
                entry.range.end + entry.gap.load(Ordering::Relaxed)
            });
        let range = start..start + size;
        self.pmap.reference();
        assert!(
            submap.set(Self::new(self.pmap, range.clone())).is_ok(),
            "vm_map: submap set twice"
        );
        let entry = ENTRY_CACHE.allocate(Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            gap: AtomicUsize::new(next - range.end),
            range,
            protection: Protection::READ | Protection::WRITE,
            max_protection: Protection::all(),
            inheritance: Inheritance::Copy,
            backing: Backing::Submap(submap.get().unwrap()),
            wire_count: 0,
            wired_access: Protection::empty(),
        })?;
        contents.link(self.bounds.start, previous, entry);
        drop(contents);
        Ok(())
    }

    /// Maps `size` bytes wherever they fit at a multiple of `align`, a
    /// multiple of the page size, wired, allowing `protection` now and at
    /// most `max_protection`; returns where. They are backed by `object` at
    /// the offset of their address in the map, or by nothing.
    ///
    /// # Errors
    ///
    /// `NoSpace` if no free space fits them, and `ResourceShortage` if no
    /// entry can be allocated.
    pub fn enter_anywhere(
        &self,
        size: usize,
        align: usize,
        protection: Protection,
        max_protection: Protection,
        object: Option<&'static Object>,
    ) -> Result<Range<usize>, Error> {
        let mut contents = self.lock();
        let (previous, start) = contents.find_space(self.bounds.start, size, align)?;
        let next = previous
            .as_ref()
            .map_or(self.bounds.start + contents.leading_gap, |entry| {
                entry.range.end + entry.gap.load(Ordering::Relaxed)
            });
        let range = start..start + size;
        let entry = ENTRY_CACHE.allocate(Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            gap: AtomicUsize::new(next - range.end),
            range: range.clone(),
            protection,
            max_protection,
            inheritance: Inheritance::Copy,
            backing: Backing::Object {
                object: AtomicPtr::new(
                    object.map_or(ptr::null_mut(), |object| ptr::from_ref(object).cast_mut()),
                ),
                offset: AtomicUsize::new(object.map_or(0, |_| start - self.bounds.start)),
            },
            wire_count: 1,
            wired_access: Protection::READ | Protection::WRITE,
        })?;
        contents.link(self.bounds.start, previous, entry);
        drop(contents);
        Ok(range)
    }

    /// Finds what `address` maps for a fault asking for `access`: the object
    /// behind it, locked, made first if its entry has none, where `address`
    /// is in it, what the entry allows, whether it is wired, and the map's
    /// version.
    ///
    /// # Errors
    ///
    /// `InvalidAddress` if no entry holds `address`.
    ///
    /// # Panics
    ///
    /// If its object can't be made. For a submap, an entry that doesn't
    /// allow `access` or a wired one: not yet.
    pub fn lookup(&self, address: usize, access: Protection) -> Result<Lookup, Error> {
        let contents = self.contents.read();
        let entry = contents.find(address).ok_or(Error::InvalidAddress)?;
        let (object, offset) = entry.checked_backing(access);
        // SAFETY: the entry holds a reference on its object, which lives
        // until vm_object_deallocate frees it.
        if let Some(object) = unsafe { object.load(Ordering::Relaxed).as_ref() } {
            // The object's token, under the map's lock.
            let state = object.lock();
            let offset = offset.load(Ordering::Relaxed);
            let lookup = entry.found(address, object, state, offset, contents.timestamp);
            drop(contents);
            return Ok(lookup);
        }
        drop(contents);
        // The read lock is lost: the lookup runs again under the write lock,
        // which makes the object if the entry still has none.
        let contents = self.lock();
        let entry = contents.find(address).ok_or(Error::InvalidAddress)?;
        let (slot, offset) = entry.checked_backing(access);
        // SAFETY: as above.
        let object = unsafe { slot.load(Ordering::Relaxed).as_ref() }.unwrap_or_else(|| {
            let object = object::allocate(entry.range.end - entry.range.start);
            slot.store(ptr::from_ref(object).cast_mut(), Ordering::Relaxed);
            offset.store(0, Ordering::Relaxed);
            object
        });
        // The object's token, under the map's lock.
        let state = object.lock();
        let offset = offset.load(Ordering::Relaxed);
        let lookup = entry.found(address, object, state, offset, contents.timestamp);
        drop(contents);
        Ok(lookup)
    }

    /// Takes its write lock, moving its version on.
    fn lock(&self) -> impl DerefMut<Target = Contents> + '_ {
        let mut contents = self.contents.write();
        contents.timestamp = contents.timestamp.wrapping_add(1);
        contents
    }
}

/// What `Map::lookup` finds for an address.
pub struct Lookup {
    /// The object behind it.
    pub object: &'static Object,
    /// The object's state, locked.
    pub state: TokenGuard<'static, ObjectState>,
    /// Where the address is in the object.
    pub offset: usize,
    /// What the entry allows.
    #[expect(dead_code, reason = "pmap_enter reads it")]
    pub protection: Protection,
    /// Whether the entry is wired.
    #[expect(dead_code, reason = "pmap_enter reads it")]
    pub wired: bool,
    /// The map's version at the lookup.
    #[expect(dead_code, reason = "vm_map_verify reads it")]
    pub version: u32,
}

impl Contents {
    /// The entry that holds `address`.
    fn find(&self, address: usize) -> Option<&Entry> {
        self.entries
            .upper_bound(Bound::Included(&address))
            .get()
            .filter(|entry| address < entry.range.end)
    }

    /// Where `size` bytes fit at a multiple of `align`, a multiple of the
    /// page size, and the entry they would follow: after the smallest gap
    /// that holds them however far aligning skips, the lowest among equals,
    /// or else from the map's `start`, if the leading gap holds them.
    fn find_space(
        &self,
        start: usize,
        size: usize,
        align: usize,
    ) -> Result<(Option<UnsafeRef<Entry>>, usize), Error> {
        // Aligning a page boundary skips at most `align - PAGE_SIZE`.
        let fit = self
            .gaps
            .lower_bound(Bound::Included(&(size + align - PAGE_SIZE, 0)));
        let aligned = start.next_multiple_of(align);
        match fit.get() {
            Some(entry) => Ok((fit.clone_pointer(), entry.range.end.next_multiple_of(align))),
            None if self.leading_gap >= aligned - start + size => Ok((None, aligned)),
            None => Err(Error::NoSpace),
        }
    }

    /// Links `entry` after `previous`, or first, keeping the gaps and size
    /// up to date; it fits between `previous` and the entry after it, or
    /// the map's `start` and end.
    fn link(&mut self, start: usize, previous: Option<UnsafeRef<Entry>>, entry: CacheBox<Entry>) {
        let (range, gap) = (entry.range.clone(), entry.gap.load(Ordering::Relaxed));
        // The entry now ends the gap after the one before it, or the leading
        // gap.
        match previous {
            Some(previous) => {
                if previous.gap_link.is_linked() {
                    let key = (previous.gap.load(Ordering::Relaxed), previous.range.start);
                    self.gaps.find_mut(&key).remove();
                }
                let previous_gap = range.start - previous.range.end;
                previous.gap.store(previous_gap, Ordering::Relaxed);
                if previous_gap != 0 {
                    self.gaps.insert(previous);
                }
            }
            None => self.leading_gap = range.start - start,
        }
        let entry = CacheBox::into_raw(entry).as_ptr();
        // SAFETY: the trees take the entry for good, once each.
        self.entries.insert(unsafe { UnsafeRef::from_raw(entry) });
        if gap != 0 {
            // SAFETY: as above.
            self.gaps.insert(unsafe { UnsafeRef::from_raw(entry) });
        }
        self.size += range.end - range.start;
    }
}

/// A range of a map and what it maps; so far only what an entry of an
/// object, wired or not, or of a submap sets.
pub struct Entry {
    /// Its place in its map's tree.
    tree_link: RBTreeAtomicLink,
    /// Its place in its map's gap tree, while free space follows it.
    gap_link: RBTreeAtomicLink,
    /// The addresses it covers.
    range: Range<usize>,
    /// The free space after it, up to the next entry or the map's end;
    /// changed only under the map's write lock.
    gap: AtomicUsize,
    /// What it allows now.
    protection: Protection,
    /// The most it may ever allow.
    max_protection: Protection,
    /// What a child task gets of it.
    inheritance: Inheritance,
    /// What backs it.
    backing: Backing,
    /// How many times it is wired: its pages stay resident and mapped.
    wire_count: u32,
    /// What it allows while wired.
    #[expect(dead_code, reason = "vm_map_pageable and vm_fault read it")]
    wired_access: Protection,
}

impl Entry {
    /// Its object, from an offset, once `vm_map_lookup`'s checks find a fault
    /// asking for `access` may be resolved in it.
    ///
    /// # Panics
    ///
    /// If it is a submap, doesn't allow `access` or is wired: not yet.
    fn checked_backing(&self, access: Protection) -> (&AtomicPtr<Object>, &AtomicUsize) {
        let Backing::Object { object, offset } = &self.backing else {
            panic!("vm_map_lookup: a submap, not yet");
        };
        assert!(
            self.protection.contains(access),
            "vm_map_lookup: KERN_PROTECTION_FAILURE, not yet"
        );
        assert!(
            self.wire_count == 0,
            "vm_map_lookup: a wired entry, not yet"
        );
        (object, offset)
    }

    /// What a lookup of `address` in it finds, with `object` behind it from
    /// `offset`, its `state` locked, at the map's `version`.
    const fn found(
        &self,
        address: usize,
        object: &'static Object,
        state: TokenGuard<'static, ObjectState>,
        offset: usize,
        version: u32,
    ) -> Lookup {
        Lookup {
            object,
            state,
            offset: address - self.range.start + offset,
            protection: self.protection,
            wired: self.wire_count != 0,
            version,
        }
    }
}

/// What backs an entry; only the kinds used here.
pub enum Backing {
    /// A map of its own over the same physical map, such as the kernel's
    /// IPC buffers.
    Submap(#[expect(dead_code, reason = "vm_map_lookup descends into it")] &'static Map),
    /// An object from `offset` on; null until it has one. Both change only
    /// under the map's write lock.
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
    EntryAdapter = UnsafeRef<Entry>: Entry { tree_link => RBTreeAtomicLink }
);

impl<'a> KeyAdapter<'a> for EntryAdapter {
    type Key = usize;

    fn get_key(&self, entry: &'a Entry) -> usize {
        entry.range.start
    }
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    GapAdapter = UnsafeRef<Entry>: Entry { gap_link => RBTreeAtomicLink }
);

impl<'a> KeyAdapter<'a> for GapAdapter {
    type Key = (usize, usize);

    fn get_key(&self, entry: &'a Entry) -> (usize, usize) {
        (entry.gap.load(Ordering::Relaxed), entry.range.start)
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
