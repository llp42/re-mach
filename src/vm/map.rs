// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Address maps.

use core::mem::ManuallyDrop;
use core::ops::{DerefMut, Range};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use intrusive_collections::rbtree::Cursor;
use intrusive_collections::{
    Bound, KeyAdapter, RBTree, RBTreeAtomicLink, UnsafeRef, intrusive_adapter,
};

use super::fault;
use super::object::{self, Flags, Object, ObjectState, Offset};
use super::page;
use super::phys_map::MachinePhysMap as _;
use super::{Inheritance, Protection, VirtualAddress, Wire};
use crate::arch::{PAGE_SIZE, PhysMap};
use crate::collections::ArrayVec;
use crate::error::Error;
use crate::reference::{Count, Counted, Reference};
use crate::slab::{Cache, CacheBox};
use crate::sync::{OnceLock, SleepLock, TokenGuard};

/// An address map: ranges of virtual memory and what they map; so far only
/// what a new map sets.
pub struct Map {
    /// The addresses it may map.
    bounds: Range<VirtualAddress>,
    /// The physical map its mappings go into.
    #[expect(clippy::struct_field_names, reason = "named for its type, PhysMap")]
    phys_map: &'static PhysMap,
    /// How many references it has.
    references: Count,
    /// What changes as it maps and unmaps, behind its lock.
    pub(super) contents: SleepLock<Contents>,
}

/// What a map's lock guards.
pub(super) struct Contents {
    /// Its entries, by address.
    entries: RBTree<EntryAdapter>,
    /// The entries free space follows, by its size, then by when it was
    /// inserted.
    gaps: RBTree<GapAdapter>,
    /// How many gaps it has inserted; each insert stamps its entry with the
    /// count before it.
    gap_inserts: u64,
    /// The free space before the first entry; all of the map while it is
    /// empty.
    leading_gap: usize,
    /// The entry whose gap a search for free space anywhere tries first;
    /// none to skip it.
    first_free: Option<UnsafeRef<Entry>>,
    /// How much it maps, in bytes.
    size: usize,
    /// How much of it is wired, in bytes.
    size_wired: usize,
    /// How much of it may never be accessed, in bytes.
    size_none: usize,
    /// Whether what it maps from now on is wired.
    wiring_required: bool,
    /// The most it may map, in bytes.
    pub(super) size_limit: usize,
    /// The most `size_limit` may be raised to.
    pub(super) max_size_limit: usize,
    /// Its version, moved on by every write lock.
    timestamp: u32,
}

impl Map {
    /// An empty map of `bounds` over `phys_map`, with one reference and no
    /// size limit.
    pub fn new(phys_map: &'static PhysMap, bounds: Range<VirtualAddress>) -> Self {
        let leading_gap = bounds.end - bounds.start;
        Self {
            bounds,
            phys_map,
            references: Count::new(1),
            contents: SleepLock::new(Contents {
                entries: RBTree::new(EntryAdapter::NEW),
                gaps: RBTree::new(GapAdapter::NEW),
                gap_inserts: 0,
                leading_gap,
                first_free: None,
                size: 0,
                size_wired: 0,
                size_none: 0,
                wiring_required: false,
                size_limit: usize::MAX,
                max_size_limit: usize::MAX,
                timestamp: 0,
            }),
        }
    }

    /// The physical map its mappings go into.
    pub const fn phys_map(&self) -> &'static PhysMap {
        self.phys_map
    }

    /// A new empty map of `bounds` over `phys_map`, a user task's, with one
    /// reference, its size limited to the memory's.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if it can't be allocated.
    pub fn create(
        phys_map: &'static PhysMap,
        bounds: Range<VirtualAddress>,
    ) -> Result<CacheBox<Self>, Error> {
        let size = page::memory_size();
        let mut map = Self::new(phys_map, bounds);
        let contents = map.contents.get_mut();
        contents.size_limit = size;
        contents.max_size_limit = size;
        MAP_CACHE.allocate(map)
    }

    /// Maps `size` bytes of `object` from `offset`, or with nothing behind
    /// them yet for none, placed as `placement` says, allowing `protection`
    /// now and at most `max_protection`, and giving child tasks
    /// `inheritance`; returns where. The entry takes `object`'s reference,
    /// which drops on failure. The entry right before them grows over them
    /// if it maps alike and their objects coalesce.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if `size` is 0; for `Placement::Fixed`,
    /// `InvalidAddress` if the range is not inside the map and `NoSpace` if
    /// it overlaps an entry; unless `max_protection` is none, as
    /// `Contents::enforce_limit`; `ResourceShortage` if no entry can be
    /// allocated.
    ///
    /// # Panics
    ///
    /// If the next entry would be extended over the range, the entry grown
    /// over it could merge with the next, or a new entry is due to be wired:
    /// not yet. As `object::coalesce`, and for `Placement::Anywhere` as
    /// `Contents::find_anywhere`.
    #[expect(
        clippy::too_many_lines,
        reason = "vm_map_enter's steps, in GNU Mach's order"
    )]
    #[expect(clippy::too_many_arguments, reason = "vm_map_enter's parameters")]
    pub fn enter(
        &self,
        placement: Placement,
        size: usize,
        object: Option<Reference<Object>>,
        offset: Offset,
        protection: Protection,
        max_protection: Protection,
        inheritance: Inheritance,
    ) -> Result<Range<VirtualAddress>, Error> {
        if size == 0 {
            return Err(Error::InvalidArgument);
        }
        // Allocated before the lock, its range set once placed; a failed
        // check frees it after. It holds `object`'s reference once linked.
        let entry = ENTRY_CACHE.allocate(Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            start: AtomicUsize::new(usize::from(self.bounds.start)),
            end: AtomicUsize::new(usize::from(self.bounds.start)),
            gap: AtomicUsize::new(0),
            gap_sequence: AtomicU64::new(0),
            protection: AtomicU32::new(protection.bits()),
            max_protection: AtomicU32::new(max_protection.bits()),
            inheritance,
            backing: Backing::Object {
                object: AtomicPtr::new(
                    object
                        .as_ref()
                        .map_or(ptr::null_mut(), |object| Reference::as_ptr(object).as_ptr()),
                ),
                offset: AtomicUsize::new(usize::from(offset)),
            },
            wire_count: AtomicU32::new(0),
            wired_access: AtomicU32::new(Protection::empty().bits()),
            in_transition: AtomicBool::new(false),
            needs_copy: AtomicBool::new(false),
            kernel_stack: false,
        })?;
        let mut contents = self.lock();
        let range = match placement {
            Placement::Fixed(start) => start
                .checked_add(size)
                .filter(|&end| start >= self.bounds.start && end <= self.bounds.end)
                .map(|end| start..end)
                .ok_or(Error::InvalidAddress)?,
            Placement::Anywhere => {
                let start = contents.find_anywhere(&self.bounds, size);
                start..start + size
            }
        };
        let below = contents.entries.upper_bound(Bound::Included(&range.start));
        let above = contents.entries.lower_bound(Bound::Excluded(&range.start));
        // Free space found anywhere overlaps nothing.
        if matches!(placement, Placement::Fixed(_))
            && (below.get().is_some_and(|entry| entry.end() > range.start)
                || above.get().is_some_and(|entry| entry.start() < range.end))
        {
            return Err(Error::NoSpace);
        }
        // Memory that may never be accessed is left out of the limit.
        if !max_protection.is_empty() {
            contents.enforce_limit(range.end - range.start)?;
        }
        let extends = |neighbour: &Entry| {
            !matches!(neighbour.backing, Backing::Submap(_))
                && !neighbour.in_transition.load(Ordering::Relaxed)
                && neighbour.inheritance == entry.inheritance
                && neighbour.protection() == entry.protection()
                && neighbour.max_protection() == entry.max_protection()
                && neighbour.wire_count() == 0
        };
        let previous = below.clone_pointer();
        let next = above.get().map_or(self.bounds.end, Entry::start);
        // The entry before grows over the range if their objects coalesce.
        if let Some(previous) = previous
            .as_ref()
            .filter(|previous| previous.end() == range.start && extends(previous))
            && let Backing::Object {
                object: slot,
                offset: previous_offset,
            } = &previous.backing
            && let Some((joined, object_offset)) = object::coalesce(
                NonNull::new(slot.load(Ordering::Relaxed))
                    .map(|previous| {
                        // SAFETY: the entry holds a reference on its object, or
                        // it is the kernel object, whose own reference never
                        // drops; this handle borrows it and never drops it.
                        ManuallyDrop::new(unsafe { Reference::from_raw(previous) })
                    })
                    .as_deref(),
                object.as_ref(),
                Offset::from(previous_offset.load(Ordering::Relaxed)),
                offset,
                previous.end() - previous.start(),
                range.end - range.start,
            )
        {
            // The grown entry holds the joined object's reference, in place
            // of its own; `object`'s drops.
            let old = slot.swap(
                joined.map_or(ptr::null_mut(), |joined| {
                    Reference::into_raw(joined).as_ptr()
                }),
                Ordering::Relaxed,
            );
            if let Some(old) = NonNull::new(old) {
                // SAFETY: the slot held that reference.
                drop(unsafe { Reference::from_raw(old) });
            }
            drop(object);
            previous_offset.store(usize::from(object_offset), Ordering::Relaxed);
            contents.size += range.end - range.start;
            if max_protection.is_empty() {
                contents.size_none += range.end - range.start;
            }
            previous
                .end
                .store(usize::from(range.end), Ordering::Relaxed);
            // Its gap shrinks to the space left before the next entry.
            if previous.gap_link.is_linked() {
                let key = (
                    previous.gap.load(Ordering::Relaxed),
                    previous.gap_sequence.load(Ordering::Relaxed),
                );
                contents.gaps.find_mut(&key).remove();
            }
            let gap = next - range.end;
            previous.gap.store(gap, Ordering::Relaxed);
            if gap != 0 {
                previous
                    .gap_sequence
                    .store(contents.gap_inserts, Ordering::Relaxed);
                contents.gap_inserts += 1;
                contents.gaps.insert(previous.clone());
            }
            // The free space hint is left as it is. The entry after may now
            // merge into the grown one.
            coalesce(&contents.entries.lower_bound(Bound::Excluded(&range.start)));
            drop(contents);
            // The entry made before the lock is unused.
            drop(entry);
            return Ok(range);
        }
        // The entry after would grow back over the range if their objects
        // coalesce.
        if let Some(next) = above
            .get()
            .filter(|next| next.start() == range.end && extends(next))
            && let Backing::Object {
                object: slot,
                offset: next_offset,
            } = &next.backing
            && object::coalesce(
                object.as_ref(),
                NonNull::new(slot.load(Ordering::Relaxed))
                    .map(|next| {
                        // SAFETY: as for the entry before.
                        ManuallyDrop::new(unsafe { Reference::from_raw(next) })
                    })
                    .as_deref(),
                offset,
                Offset::from(next_offset.load(Ordering::Relaxed)),
                range.end - range.start,
                next.end() - next.start(),
            )
            .is_some()
        {
            panic!("vm_map_enter: extending the next entry, not yet");
        }
        entry
            .start
            .store(usize::from(range.start), Ordering::Relaxed);
        entry.end.store(usize::from(range.end), Ordering::Relaxed);
        entry.gap.store(next - range.end, Ordering::Relaxed);
        // Its slot, set above, now holds the reference.
        if let Some(object) = object {
            Reference::into_raw(object);
        }
        // The free space hint moves on to an entry right at its end.
        let follows_first_free = contents
            .first_free
            .as_ref()
            .map_or(self.bounds.start, |entry| entry.end())
            == range.start;
        let entry = contents.link(self.bounds.start, previous, entry);
        if max_protection.is_empty() {
            contents.size_none += range.end - range.start;
        }
        if follows_first_free {
            contents.first_free = Some(entry);
        }
        assert!(
            !contents.wiring_required,
            "vm_map_enter: wiring_required, not yet"
        );
        drop(contents);
        Ok(range)
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
                entry.end() + entry.gap.load(Ordering::Relaxed)
            });
        let range = start..start + size;
        // The free space hint moves on to an entry right at its end.
        let follows_first_free = contents
            .first_free
            .as_ref()
            .map_or(self.bounds.start, |entry| entry.end())
            == range.start;
        self.phys_map.reference();
        assert!(
            submap.set(Self::new(self.phys_map, range.clone())).is_ok(),
            "vm_map: submap set twice"
        );
        let entry = ENTRY_CACHE.allocate(Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            start: AtomicUsize::new(usize::from(range.start)),
            end: AtomicUsize::new(usize::from(range.end)),
            gap: AtomicUsize::new(next - range.end),
            gap_sequence: AtomicU64::new(0),
            protection: AtomicU32::new((Protection::READ | Protection::WRITE).bits()),
            max_protection: AtomicU32::new(Protection::all().bits()),
            inheritance: Inheritance::Copy,
            backing: Backing::Submap(submap.get().unwrap()),
            wire_count: AtomicU32::new(0),
            wired_access: AtomicU32::new(Protection::empty().bits()),
            in_transition: AtomicBool::new(false),
            needs_copy: AtomicBool::new(false),
            kernel_stack: false,
        })?;
        let entry = contents.link(self.bounds.start, previous, entry);
        if follows_first_free {
            contents.first_free = Some(entry);
        }
        drop(contents);
        Ok(())
    }

    /// Maps `size` bytes wherever they fit at a multiple of `align`, a
    /// multiple of the page size, wired, allowing `protection` now and at
    /// most `max_protection`; returns where. They are backed by `object` at
    /// the offset of their address in the map, or by nothing; the entry
    /// holds a reference on `object`, the kernel object. They count in the
    /// map's wired size, with an object or without (GNU Mach leaves them
    /// out). `usage` tags a kernel stack's entry (no GNU Mach counterpart).
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
        usage: Usage,
    ) -> Result<Range<VirtualAddress>, Error> {
        let mut contents = self.lock();
        let (previous, start) = contents.find_space(self.bounds.start, size, align)?;
        let next = previous
            .as_ref()
            .map_or(self.bounds.start + contents.leading_gap, |entry| {
                entry.end() + entry.gap.load(Ordering::Relaxed)
            });
        let range = start..start + size;
        let entry = ENTRY_CACHE.allocate(Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            start: AtomicUsize::new(usize::from(range.start)),
            end: AtomicUsize::new(usize::from(range.end)),
            gap: AtomicUsize::new(next - range.end),
            gap_sequence: AtomicU64::new(0),
            protection: AtomicU32::new(protection.bits()),
            max_protection: AtomicU32::new(max_protection.bits()),
            inheritance: Inheritance::Copy,
            backing: Backing::Object {
                object: AtomicPtr::new(
                    object.map_or(ptr::null_mut(), |object| ptr::from_ref(object).cast_mut()),
                ),
                offset: AtomicUsize::new(object.map_or(0, |_| start - self.bounds.start)),
            },
            wire_count: AtomicU32::new(1),
            wired_access: AtomicU32::new((Protection::READ | Protection::WRITE).bits()),
            in_transition: AtomicBool::new(false),
            needs_copy: AtomicBool::new(false),
            kernel_stack: matches!(usage, Usage::KernelStack),
        })?;
        if let Some(object) = object {
            // SAFETY: it is the kernel object, whose own reference never
            // drops; this handle borrows it and never drops it.
            let held = ManuallyDrop::new(unsafe { Reference::from_raw(NonNull::from(object)) });
            // vm_object_reference: another, held by the entry.
            Reference::into_raw(Reference::clone(&held));
        }
        contents.size_wired += size;
        // The free space hint moves on to it.
        contents.first_free = Some(contents.link(self.bounds.start, previous, entry));
        drop(contents);
        Ok(range)
    }

    /// Sets what `range`, cut to the map's bounds, allows to `protection`,
    /// as `change` says, in its physical map too, splitting the entries its
    /// start and end fall inside.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if a submap lies in `range`, and `ProtectionFailure`
    /// if an entry there may never allow `protection`; an entry split before
    /// that stays split.
    ///
    /// # Panics
    ///
    /// If no entry can be allocated. If an entry would merge with the one
    /// before it, one is in transition, has wired access or is wired, or the
    /// map wires what it maps: not yet. As `Contents::clip_start`,
    /// `Contents::clip_end` and `MachinePhysMap::protect`.
    pub fn protect(
        &self,
        range: Range<VirtualAddress>,
        protection: Protection,
        change: Change,
    ) -> Result<(), Error> {
        // Taken before the lock, for the splits.
        let mut reservation = EntryReservation::new();
        let mut guard = self.lock();
        let contents = &mut *guard;
        let end = range.end.min(self.bounds.end);
        let start = range.start.max(self.bounds.start).min(end);
        if let Some(entry) = contents
            .entries
            .upper_bound(Bound::Included(&start))
            .clone_pointer()
            .filter(|entry| start < entry.end())
            && start > entry.start()
        {
            contents.clip_start(&entry, start, &mut reservation);
        }
        // Every entry is checked before any changes, from the one starting
        // at `start`, else the next.
        let mut current = contents.entries.lower_bound(Bound::Included(&start));
        while let Some(entry) = current.get().filter(|entry| entry.start() < end) {
            if matches!(entry.backing, Backing::Submap(_)) {
                return Err(Error::InvalidArgument);
            }
            assert!(
                !entry.in_transition.load(Ordering::Relaxed),
                "vm_map_protect: vm_map_entry_wait, not yet"
            );
            if !entry.max_protection().contains(protection) {
                return Err(Error::ProtectionFailure);
            }
            current.move_next();
        }
        // Each entry is found by address, as splitting it changes the tree.
        let mut address = start;
        while let Some(entry) = contents
            .entries
            .lower_bound(Bound::Included(&address))
            .clone_pointer()
            .filter(|entry| entry.start() < end)
        {
            if end < entry.end() {
                contents.clip_end(&entry, end, &mut reservation);
            }
            let old = entry.protection();
            match change {
                Change::Current => entry.protection.store(protection.bits(), Ordering::Relaxed),
                Change::Maximum => {
                    // Memory that may never be accessed is left out of the
                    // limit.
                    if entry.max_protection() != protection && protection.is_empty() {
                        contents.size_none += entry.end() - entry.start();
                    }
                    entry
                        .max_protection
                        .store(protection.bits(), Ordering::Relaxed);
                    entry
                        .protection
                        .store((protection & old).bits(), Ordering::Relaxed);
                }
            }
            let new = entry.protection();
            assert!(
                new.is_empty() || entry.wired_access().is_empty(),
                "vm_map_protect: an entry with wired access, not yet"
            );
            assert!(
                new.is_empty() || !contents.wiring_required,
                "vm_map_protect: wiring_required, not yet"
            );
            if new != old {
                self.phys_map
                    .protect(usize::from(entry.start())..usize::from(entry.end()), new);
            }
            address = entry.end();
            coalesce(&contents.entries.find(&entry.start()));
        }
        // The entry after the range, with the last in it.
        coalesce(&contents.entries.lower_bound(Bound::Included(&address)));
        // vm_map_pageable_scan; no entry merged, so from the entry starting
        // at `start`, else the next.
        let mut first = contents.entries.lower_bound(Bound::Included(&start));
        while let Some(entry) = first.get().filter(|entry| entry.start() < end) {
            assert!(
                entry.wired_access().is_empty(),
                "vm_map_pageable_scan: an entry with wired access, not yet"
            );
            assert!(
                entry.wire_count() == 0,
                "vm_map_pageable_scan: vm_fault_unwire, not yet"
            );
            first.move_next();
        }
        drop(guard);
        Ok(())
    }

    /// Sets what the entries of `range`, cut to the map's bounds, allow
    /// while wired to `access`, splitting them at its end, then unwires
    /// those given none and wires those newly given some; with
    /// `RangeCheck::Checked`, the range holds no hole, and each entry allows
    /// `access`.
    ///
    /// # Errors
    ///
    /// As `Map::pageable_locked`.
    ///
    /// # Panics
    ///
    /// If no entry can be allocated. As `Map::pageable_locked`.
    pub fn pageable(
        &self,
        range: Range<VirtualAddress>,
        access: Protection,
        check: RangeCheck,
    ) -> Result<(), Error> {
        // Taken before the lock, for the splits to use.
        let mut reservation = EntryReservation::new();
        let contents = self.lock();
        self.pageable_locked(contents, range, access, check, &mut reservation)
            .map_err(|(contents, error)| {
                drop(contents);
                error
            })
    }

    /// Wires all the map maps now, as `flags` say, for reading and writing
    /// even where an entry allows less, and what it maps from now on; for
    /// none, unwires it all and stops wiring what it maps from now on.
    ///
    /// # Errors
    ///
    /// As `Map::pageable_locked`; with `Wire::FUTURE`, whether the map wires
    /// what it maps from now on is restored.
    ///
    /// # Panics
    ///
    /// If no entry can be allocated. For an empty map with `Wire::CURRENT`
    /// or none: not yet. As `Map::pageable_locked`.
    pub fn pageable_all(&self, flags: Wire) -> Result<(), Error> {
        // Taken before the lock, for the splits, which the whole map never
        // needs.
        let mut reservation = EntryReservation::new();
        let mut contents = self.lock();
        let wiring_required = contents.wiring_required;
        let access = if flags.is_empty() {
            contents.wiring_required = false;
            Protection::empty()
        } else {
            if flags.contains(Wire::FUTURE) {
                contents.wiring_required = true;
            }
            if !flags.contains(Wire::CURRENT) {
                drop(contents);
                return Ok(());
            }
            Protection::READ | Protection::WRITE
        };
        // vm_map_pageable_current: from the first entry's start to the last
        // entry's end, unchecked.
        let start = contents.entries.front().get().map(Entry::start);
        let end = contents.entries.back().get().map(Entry::end);
        let (Some(start), Some(end)) = (start, end) else {
            panic!("vm_map_pageable_current: an empty map, not yet");
        };
        self.pageable_locked(
            contents,
            start..end,
            access,
            RangeCheck::Unchecked,
            &mut reservation,
        )
        .map_err(|(mut contents, error)| {
            if flags.contains(Wire::FUTURE) {
                contents.wiring_required = wiring_required;
            }
            drop(contents);
            error
        })
    }

    /// `Map::pageable` with `guard`, the map's contents under its write
    /// lock, which it drops on success, and `reservation` for the splits,
    /// taken before the lock.
    ///
    /// # Errors
    ///
    /// `NoSpace`, with `guard` back, if no entry holds the range's start,
    /// and with `RangeCheck::Checked` if a hole lies in the range or an
    /// entry there doesn't allow `access`; an entry split before that stays
    /// split.
    ///
    /// # Panics
    ///
    /// If the range starts inside an entry, an entry there is in transition,
    /// a wired entry allows nothing, or an entry to wire is a submap or needs
    /// a copy and allows writing: not yet. As `Contents::clip_end`,
    /// `object::allocate`, `fault::unwire` and `fault::wire`.
    #[expect(
        clippy::too_many_lines,
        reason = "vm_map_pageable's and vm_map_pageable_scan's steps, in GNU Mach's order"
    )]
    fn pageable_locked<G: DerefMut<Target = Contents>>(
        &self,
        mut guard: G,
        range: Range<VirtualAddress>,
        access: Protection,
        check: RangeCheck,
        reservation: &mut EntryReservation,
    ) -> Result<(), (G, Error)> {
        let contents = &mut *guard;
        let end = range.end.min(self.bounds.end);
        let start = range.start.max(self.bounds.start).min(end);
        let Some(first) = contents.find(start).map(Entry::start) else {
            return Err((guard, Error::NoSpace));
        };
        assert!(
            first == start,
            "vm_map_clip_start: splitting an entry, not yet"
        );
        // Pass 1: the last entry is split at the end; if checked, no hole,
        // and every entry allows `access`.
        let mut address = start;
        while let Some(entry) = contents
            .entries
            .lower_bound(Bound::Included(&address))
            .clone_pointer()
            .filter(|entry| entry.start() < end)
        {
            assert!(
                !entry.in_transition.load(Ordering::Relaxed),
                "vm_map_pageable: vm_map_entry_wait, not yet"
            );
            if end < entry.end() {
                contents.clip_end(&entry, end, reservation);
            }
            let next = contents
                .entries
                .lower_bound(Bound::Included(&entry.end()))
                .get()
                .map(Entry::start);
            if matches!(check, RangeCheck::Checked)
                && ((entry.end() < end && next.is_none_or(|next| next > entry.end()))
                    || !entry.protection().contains(access))
            {
                return Err((guard, Error::NoSpace));
            }
            address = entry.end();
        }
        // Pass 2: the wired access.
        let mut cursor = contents.entries.lower_bound(Bound::Included(&start));
        while let Some(entry) = cursor.get().filter(|entry| entry.start() < end) {
            entry.wired_access.store(access.bits(), Ordering::Relaxed);
            cursor.move_next();
        }
        // vm_map_pageable_scan, pass 1: the counts, and whether any entry
        // needs wiring faults.
        let mut wire_faults = false;
        let mut cursor = contents.entries.lower_bound(Bound::Included(&start));
        while let Some(entry) = cursor.get().filter(|entry| entry.start() < end) {
            cursor.move_next();
            if entry.wired_access().is_empty() {
                if entry.wire_count() != 0 {
                    // vm_map_entry_reset_wired.
                    contents.size_wired -= entry.end() - entry.start();
                    entry.wire_count.store(0, Ordering::Relaxed);
                    fault::unwire(self, entry);
                }
                continue;
            }
            if entry.protection().is_empty() {
                assert!(
                    entry.wire_count() == 0,
                    "vm_map_pageable_scan: vm_fault_unwire after a change of protection, not yet"
                );
                continue;
            }
            if entry.wire_count() == 0 {
                assert!(
                    !entry.needs_copy.load(Ordering::Relaxed)
                        || !entry.protection().contains(Protection::WRITE),
                    "vm_map_pageable_scan: vm_object_shadow, not yet"
                );
                // One with no object gets one.
                let Backing::Object { object, offset } = &entry.backing else {
                    panic!("vm_map_lookup: a submap, not yet");
                };
                if object.load(Ordering::Relaxed).is_null() {
                    let new = object::allocate(entry.end() - entry.start());
                    // The entry holds its reference.
                    object.store(Reference::into_raw(new).as_ptr(), Ordering::Relaxed);
                    offset.store(0, Ordering::Relaxed);
                }
            }
            // vm_map_entry_inc_wired: only a first wiring faults, so the
            // count stops at 2.
            let count = entry.wire_count();
            if count <= 1 {
                if count == 0 {
                    contents.size_wired += entry.end() - entry.start();
                }
                entry.wire_count.store(count + 1, Ordering::Relaxed);
            }
            wire_faults |= entry.wire_count() == 1;
        }
        if !wire_faults {
            drop(guard);
            return Ok(());
        }
        // Pass 2: faults lock the map, so it is unlocked meanwhile, its
        // entries in transition.
        let mut cursor = contents.entries.lower_bound(Bound::Included(&start));
        // Pass 1 found none of them in transition.
        while let Some(entry) = cursor.get().filter(|entry| entry.end() <= end) {
            // Nothing waits for an entry in transition yet.
            entry.in_transition.store(true, Ordering::Relaxed);
            cursor.move_next();
        }
        drop(guard);
        // Entries in transition stay put; each is found by address.
        let mut address = start;
        loop {
            let contents = self.contents.read();
            let entry = contents
                .entries
                .lower_bound(Bound::Included(&address))
                .clone_pointer()
                .filter(|entry| entry.end() <= end);
            drop(contents);
            let Some(entry) = entry else {
                break;
            };
            // Only this call left a count at 1.
            if entry.wire_count() == 1 {
                fault::wire(self, &entry);
            }
            address = entry.end();
        }
        let contents = self.lock();
        let mut cursor = contents.entries.lower_bound(Bound::Included(&start));
        while let Some(entry) = cursor.get().filter(|entry| entry.end() <= end) {
            // An entry linked into a hole while the map was unlocked never was.
            if entry.in_transition.load(Ordering::Relaxed) {
                entry.in_transition.store(false, Ordering::Relaxed);
            }
            cursor.move_next();
        }
        drop(contents);
        Ok(())
    }

    /// Removes the entries of `range` from the map, `contents` its contents
    /// under its write lock, and their pages from its physical map,
    /// splitting entries at the range's start and end with ones from
    /// `reservation`.
    ///
    /// # Panics
    ///
    /// If an entry there is in transition, a submap, wired or the kernel
    /// object's: not yet. As `Contents::clip_start`, `Contents::clip_end`
    /// and `MachinePhysMap::remove`; at an object's last reference, as its
    /// free.
    fn delete(
        &self,
        contents: &mut Contents,
        range: Range<VirtualAddress>,
        reservation: &mut EntryReservation,
    ) {
        if let Some(entry) = contents
            .entries
            .upper_bound(Bound::Included(&range.start))
            .clone_pointer()
            .filter(|entry| range.start < entry.end())
            && range.start > entry.start()
        {
            contents.clip_start(&entry, range.start, reservation);
        }
        // The free space hint moves before the range.
        let first = contents.entries.lower_bound(Bound::Included(&range.start));
        if contents
            .first_free
            .as_ref()
            .map_or(self.bounds.start, |entry| entry.start())
            >= range.start
        {
            contents.first_free = first.peek_prev().clone_pointer();
        }
        let mut address = range.start;
        while let Some(entry) = contents
            .entries
            .lower_bound(Bound::Included(&address))
            .clone_pointer()
            .filter(|entry| entry.start() < range.end)
        {
            if range.end < entry.end() {
                contents.clip_end(&entry, range.end, reservation);
            }
            assert!(
                !entry.in_transition.load(Ordering::Relaxed),
                "vm_map_delete: vm_map_entry_wait, not yet"
            );
            address = entry.end();
            // vm_map_entry_delete.
            let Backing::Object { object, offset } = &entry.backing else {
                panic!("vm_map_entry_delete: a submap, not yet");
            };
            if let Some(pointer) = NonNull::new(object.load(Ordering::Relaxed)) {
                // SAFETY: the entry holds a reference on its object, which
                // lives until that reference drops below.
                let object = unsafe { pointer.as_ref() };
                assert!(
                    entry.wire_count() == 0,
                    "vm_map_entry_delete: vm_fault_unwire, not yet"
                );
                assert!(
                    !ptr::eq(object, &raw const object::KERNEL_OBJECT),
                    "vm_map_entry_delete: the kernel object, not yet"
                );
                // Entries are shared only by vm_map_fork, not yet.
                self.phys_map
                    .remove(usize::from(entry.start())..usize::from(entry.end()));
                // With no pager and no other reference, its pages go now.
                let mut state = object.lock();
                if !state.flags.contains(Flags::PAGER_CREATED)
                    && object.references() == 1
                    && state.paging_in_progress == 0
                {
                    let offset = offset.load(Ordering::Relaxed);
                    state.remove_pages(offset..offset + (entry.end() - entry.start()));
                }
                drop(state);
                // vm_object_deallocate: the entry's reference goes.
                // SAFETY: the entry held it, and goes too.
                drop(unsafe { Reference::from_raw(pointer) });
            }
            contents.size -= entry.end() - entry.start();
            if entry.max_protection().is_empty() {
                contents.size_none -= entry.end() - entry.start();
            }
            // SAFETY: the hint moved off the range above, the entry is not in
            // transition, and `entry` is not used again.
            drop(unsafe { contents.unlink(&self.bounds, &entry) });
        }
    }

    /// Removes the entries of `range`, cut to the map's bounds, from the map,
    /// and their pages from its physical map.
    ///
    /// # Panics
    ///
    /// If no entry can be allocated. As `Map::delete`.
    pub fn remove(&self, range: Range<VirtualAddress>) {
        // Taken before the lock, for the splits.
        let mut reservation = EntryReservation::new();
        let mut guard = self.lock();
        let end = range.end.min(self.bounds.end);
        let start = range.start.max(self.bounds.start).min(end);
        self.delete(&mut guard, start..end, &mut reservation);
        drop(guard);
    }

    /// Copies `range` out of the map into a new copy, holding entries for
    /// it, which take references on their objects, as `vm_map_copyin` does:
    /// with `Source::Destroy` the range then leaves the map; with
    /// `Source::Keep` the copy and the map share each object until either
    /// writes, and the map's mappings there stop allowing writing.
    ///
    /// # Panics
    ///
    /// If the copy or an entry can't be allocated, or the copy's entries and
    /// the splits at the range's ends need more than four. If `range` is
    /// empty or wraps, starts where no entry is, holds a hole, or an entry
    /// there doesn't allow reading or is a submap; if an object is not
    /// temporary; for `Source::Keep`, if an entry is wired or has no object:
    /// not yet. As `Contents::clip_start`, `Contents::clip_end`,
    /// `MachinePhysMap::protect` and `Map::delete`.
    #[expect(
        clippy::too_many_lines,
        reason = "vm_map_copyin's steps, in GNU Mach's order"
    )]
    pub fn copy_in(&self, range: Range<VirtualAddress>, source: Source) -> CacheBox<MapCopy> {
        assert!(
            range.start != range.end,
            "vm_map_copyin: zero bytes, not yet"
        );
        let start = range.start.align_down(PAGE_SIZE);
        let end = range.end.next_multiple_of(PAGE_SIZE);
        assert!(
            range.start < range.end && start < end,
            "vm_map_copyin: KERN_INVALID_ADDRESS, not yet"
        );
        // The header, before the lock; it keeps the range unrounded.
        let mut copy = COPY_CACHE
            .allocate(MapCopy {
                kind: CopyKind::Entries,
                offset: range.start,
                size: range.end - range.start,
                entries: RBTree::new(EntryAdapter::NEW),
            })
            .unwrap_or_else(|error| panic!("vm_map_copyin: {error:?}"));
        // Taken before the lock, for the copy's entries and the splits.
        let mut reservation = EntryReservation::new();
        let mut guard = self.lock();
        let contents = &mut *guard;
        let mut entry = contents
            .entries
            .upper_bound(Bound::Included(&start))
            .clone_pointer()
            .filter(|entry| start < entry.end())
            .unwrap_or_else(|| panic!("vm_map_copyin: KERN_INVALID_ADDRESS, not yet"));
        if start > entry.start() {
            contents.clip_start(&entry, start, &mut reservation);
        }
        loop {
            assert!(
                entry.protection().contains(Protection::READ),
                "vm_map_copyin: KERN_PROTECTION_FAILURE, not yet"
            );
            if end < entry.end() {
                contents.clip_end(&entry, end, &mut reservation);
            }
            let Backing::Object { object, offset } = &entry.backing else {
                panic!("vm_map_copyin: a submap, not yet");
            };
            let object = object.load(Ordering::Relaxed);
            let wired = entry.wire_count() != 0;
            // vm_map_entry_copy: unwired, out of transition, no kernel stack,
            // and needing a copy if the source does.
            let mut new = reservation.take();
            *new = Entry {
                tree_link: RBTreeAtomicLink::new(),
                gap_link: RBTreeAtomicLink::new(),
                start: AtomicUsize::new(usize::from(entry.start())),
                end: AtomicUsize::new(usize::from(entry.end())),
                gap: AtomicUsize::new(entry.gap.load(Ordering::Relaxed)),
                gap_sequence: AtomicU64::new(0),
                protection: AtomicU32::new(entry.protection().bits()),
                max_protection: AtomicU32::new(entry.max_protection().bits()),
                inheritance: entry.inheritance,
                backing: Backing::Object {
                    object: AtomicPtr::new(object),
                    offset: AtomicUsize::new(offset.load(Ordering::Relaxed)),
                },
                wire_count: AtomicU32::new(0),
                wired_access: AtomicU32::new(Protection::empty().bits()),
                in_transition: AtomicBool::new(false),
                needs_copy: AtomicBool::new(entry.needs_copy.load(Ordering::Relaxed)),
                kernel_stack: false,
            };
            let held = NonNull::new(object).map(|object| {
                // SAFETY: the entry holds a reference on its object, or it is
                // the kernel object, whose own reference never drops; this
                // handle borrows it and never drops it.
                ManuallyDrop::new(unsafe { Reference::from_raw(object) })
            });
            // A source that goes moves a temporary object's reference, or no
            // object, to the copy; GNU Mach's use_shared_copy is set only by
            // vm_map_fork.
            if matches!(source, Source::Destroy)
                && held
                    .as_ref()
                    .is_none_or(|object| object.lock().flags.contains(Flags::TEMPORARY))
            {
                // Another reference, held by the copy's entry, as deleting the
                // source drops its own.
                if let Some(object) = &held {
                    Reference::into_raw(Reference::clone(object));
                }
            } else {
                assert!(!wired, "vm_map_copyin: vm_object_copy_slowly, not yet");
                let object = held
                    .as_ref()
                    .unwrap_or_else(|| panic!("vm_object_copy_temporary: VM_OBJECT_NULL, not yet"));
                let shared = object::copy_temporary(object).unwrap_or_else(|| {
                    panic!("vm_map_copyin: vm_object_copy_strategically, not yet")
                });
                // The copy's entry holds it; both entries shadow the object
                // before writing.
                Reference::into_raw(shared);
                new.needs_copy.store(true, Ordering::Relaxed);
                if !entry.needs_copy.load(Ordering::Relaxed) {
                    // vm_object_pmap_protect, over the entry's range.
                    self.phys_map.protect(
                        usize::from(entry.start())..usize::from(entry.end()),
                        entry.protection() - Protection::WRITE,
                    );
                    entry.needs_copy.store(true, Ordering::Relaxed);
                }
            }
            let copied = new.end();
            let new = CacheBox::into_raw(new).as_ptr();
            // SAFETY: the copy's tree takes the entry, once, until a map takes
            // it or the copy is discarded.
            copy.entries.insert(unsafe { UnsafeRef::from_raw(new) });
            if copied >= end {
                break;
            }
            entry = contents
                .entries
                .lower_bound(Bound::Included(&copied))
                .clone_pointer()
                .filter(|next| next.start() == copied)
                .unwrap_or_else(|| panic!("vm_map_copyin: a hole, KERN_INVALID_ADDRESS, not yet"));
        }
        if matches!(source, Source::Destroy) {
            self.delete(contents, start..end, &mut reservation);
        }
        drop(guard);
        copy
    }

    /// Moves `copy`'s unwired entries into the map wherever they fit,
    /// allowing reading and writing, as `vm_map_copyout` does for an entry
    /// list; returns where its memory starts.
    ///
    /// # Errors
    ///
    /// As `Contents::enforce_limit`; the copy comes back with the error.
    ///
    /// # Panics
    ///
    /// If the copy holds an object or pages, or a wired entry, or the map
    /// wires what it maps: not yet. As `Contents::find_anywhere`.
    pub fn copy_out(
        &self,
        mut copy: CacheBox<MapCopy>,
    ) -> Result<VirtualAddress, (CacheBox<MapCopy>, Error)> {
        match copy.kind {
            CopyKind::Entries => {}
            CopyKind::Object => panic!("vm_map_copyout: VM_MAP_COPY_OBJECT, not yet"),
            CopyKind::Pages => panic!("vm_map_copyout: vm_map_copyout_page_list, not yet"),
        }
        let copy_start = copy.offset.align_down(PAGE_SIZE);
        let size = (copy.offset + copy.size).next_multiple_of(PAGE_SIZE) - copy_start;
        let mut contents = self.lock();
        let start = contents.find_anywhere(&self.bounds, size);
        if let Err(error) = contents.enforce_limit(size) {
            drop(contents);
            return Err((copy, error));
        }
        let last = contents
            .entries
            .upper_bound(Bound::Included(&start))
            .clone_pointer();
        let next = last
            .as_ref()
            .map_or(self.bounds.start + contents.leading_gap, |entry| {
                entry.end() + entry.gap.load(Ordering::Relaxed)
            });
        // The free space hint moves on to the last entry if it was the one
        // before them.
        let follows_first_free = contents
            .first_free
            .as_ref()
            .map_or(self.bounds.start, |entry| entry.end())
            == start;
        // Each entry is moved and reset out of the copy's tree, as it is
        // linked; GNU Mach does both before vm_map_copy_insert. `link` adds
        // its size to the map's.
        let mut previous = last;
        while let Some(entry) = copy.entries.front_mut().remove() {
            let entry = NonNull::new(UnsafeRef::into_raw(entry)).unwrap();
            // SAFETY: a copy's entries come from `CacheBox::into_raw` of
            // `ENTRY_CACHE`'s boxes (`Map::copy_in`), and its tree held the
            // only pointer to each; the copy has one owner and its entries'
            // gap links are unused, so out of the tree, it is the box's alone.
            let mut entry = unsafe { CacheBox::from_raw(&ENTRY_CACHE, entry) };
            assert!(
                entry.wire_count() == 0,
                "vm_map_copyout: a wired entry, PMAP_ENTER, not yet"
            );
            let range = start + (entry.start() - copy_start)..start + (entry.end() - copy_start);
            entry
                .start
                .store(usize::from(range.start), Ordering::Relaxed);
            entry.end.store(usize::from(range.end), Ordering::Relaxed);
            entry.inheritance = Inheritance::Copy;
            entry.protection.store(
                (Protection::READ | Protection::WRITE).bits(),
                Ordering::Relaxed,
            );
            entry
                .max_protection
                .store(Protection::all().bits(), Ordering::Relaxed);
            // The copy kept its source's gap; the free space after it now
            // reaches the next entry.
            entry.gap.store(next - range.end, Ordering::Relaxed);
            previous = Some(contents.link(self.bounds.start, previous, entry));
        }
        if follows_first_free {
            contents.first_free = previous;
        }
        let address = start + (copy.offset - copy_start);
        // vm_map_copy_insert frees the header.
        drop(copy);
        assert!(
            !contents.wiring_required,
            "vm_map_copyout: wiring_required, not yet"
        );
        drop(contents);
        Ok(address)
    }

    /// Writes `copy`'s memory over the map from `address`, as
    /// `vm_map_copy_overwrite` does: the destination's entry takes the copy's
    /// object, offset and need of a copy, and its old pages leave the
    /// physical map. Consumes the copy; nothing for none.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if the copy's start or size, or `address`, is not
    /// page-aligned, `InvalidAddress` if no entry holds `address`, and
    /// `ProtectionFailure` if its entry doesn't allow writing.
    ///
    /// # Panics
    ///
    /// If the copy holds an object or pages. If an entry can't be allocated. If
    /// the destination's entry is in transition, ends before the copy's
    /// memory, differs from the copy's entry in size, is a submap, is wired,
    /// or holds an object that is not temporary: not yet. On an error, as
    /// discarding a copy holding entries: not yet. As `Contents::clip_start`,
    /// `MachinePhysMap::protect` and freeing the old object at its last
    /// reference.
    pub fn copy_overwrite(
        &self,
        address: VirtualAddress,
        copy: Option<CacheBox<MapCopy>>,
    ) -> Result<(), Error> {
        let Some(mut copy) = copy else {
            return Ok(());
        };
        assert!(
            matches!(copy.kind, CopyKind::Entries),
            "vm_map_copy_overwrite: not an entry list"
        );
        if !usize::from(copy.offset).is_multiple_of(PAGE_SIZE)
            || !copy.size.is_multiple_of(PAGE_SIZE)
            || !usize::from(address).is_multiple_of(PAGE_SIZE)
        {
            return Err(Error::InvalidArgument);
        }
        let size = copy.size;
        if size == 0 {
            // vm_map_copy_discard.
            drop(copy);
            return Ok(());
        }
        // Taken before the lock, for the split.
        let mut reservation = EntryReservation::new();
        // Pass 1: the destination allows writing.
        let mut guard = self.lock();
        let contents = &mut *guard;
        let Some(entry) = contents
            .entries
            .upper_bound(Bound::Included(&address))
            .clone_pointer()
            .filter(|entry| address < entry.end())
        else {
            drop(guard);
            return Err(Error::InvalidAddress);
        };
        if address > entry.start() {
            contents.clip_start(&entry, address, &mut reservation);
        }
        if !entry.protection().contains(Protection::WRITE) {
            drop(guard);
            return Err(Error::ProtectionFailure);
        }
        assert!(
            !entry.in_transition.load(Ordering::Relaxed),
            "vm_map_copy_overwrite: vm_map_entry_wait, not yet"
        );
        let entry_size = entry.end() - entry.start();
        assert!(
            size <= entry_size,
            "vm_map_copy_overwrite: several entries, not yet"
        );
        // Pass 2, over the copy's one entry, as the destination's is one.
        let copy_entry = copy.entries.front().clone_pointer().unwrap();
        let copy_size = copy_entry.end() - copy_entry.start();
        assert!(
            copy_size >= entry_size,
            "vm_map_copy_overwrite: vm_map_clip_end, not yet"
        );
        assert!(
            entry_size >= copy_size,
            "vm_map_copy_overwrite: vm_map_copy_clip_end, not yet"
        );
        let Backing::Object { object, offset } = &entry.backing else {
            panic!("vm_map_copy_overwrite: a submap, not yet");
        };
        let old = NonNull::new(object.load(Ordering::Relaxed));
        // Entries are shared only by vm_map_fork, not yet: temporary memory is
        // thrown away and the copy's put in its place.
        assert!(
            old.is_none_or(|old| {
                // SAFETY: the entry holds a reference on its object, which the
                // map's lock keeps.
                unsafe { old.as_ref() }
                    .lock()
                    .flags
                    .contains(Flags::TEMPORARY)
            }),
            "vm_map_copy_overwrite: vm_fault_copy, not yet"
        );
        let Backing::Object {
            object: copy_object,
            offset: copy_offset,
        } = &copy_entry.backing
        else {
            panic!("vm_map_copy_overwrite: a copy's submap");
        };
        object.store(copy_object.load(Ordering::Relaxed), Ordering::Relaxed);
        offset.store(copy_offset.load(Ordering::Relaxed), Ordering::Relaxed);
        entry.needs_copy.store(
            copy_entry.needs_copy.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        assert!(
            entry.wire_count() == 0,
            "vm_map_entry_reset_wired: a wired entry, not yet"
        );
        // The copy's entry goes; its reference is now the entry's.
        let copy_entry = copy.entries.front_mut().remove().unwrap();
        let copy_entry = NonNull::new(UnsafeRef::into_raw(copy_entry)).unwrap();
        // SAFETY: a copy's entries come from `CacheBox::into_raw` of
        // `ENTRY_CACHE`'s boxes (`Map::copy_in`), and its tree held the only
        // pointer to each; out of the tree, it is the box's alone.
        drop(unsafe { CacheBox::from_raw(&ENTRY_CACHE, copy_entry) });
        if let Some(old) = old {
            // vm_object_pmap_protect to none: the old pages leave the
            // physical map.
            self.phys_map.protect(
                usize::from(entry.start())..usize::from(entry.end()),
                Protection::empty(),
            );
            // vm_object_deallocate: the entry's old reference goes.
            // SAFETY: the entry held it, and now holds the copy's.
            drop(unsafe { Reference::from_raw(old) });
        }
        drop(guard);
        // vm_map_copy_discard: the copy is empty.
        drop(copy);
        Ok(())
    }

    /// Finds what `address` maps for a fault asking for `access`: the object
    /// behind it, locked, shadowed first if `access` writes and its entry
    /// needs a copy, or made first if its entry has none, with a reference
    /// taken under the map's lock for the fault, where `address` is in it,
    /// what the entry allows, whether it is wired, and the map's version.
    ///
    /// # Errors
    ///
    /// `InvalidAddress` if no entry holds `address`.
    ///
    /// # Panics
    ///
    /// If its object or shadow can't be made. For a submap or an entry that
    /// doesn't allow `access`: not yet.
    pub fn lookup(&self, address: VirtualAddress, access: Protection) -> Result<Lookup, Error> {
        let contents = self.contents.read();
        let entry = contents.find(address).ok_or(Error::InvalidAddress)?;
        let (object, offset) = entry.checked_backing(access);
        let shadows =
            access.contains(Protection::WRITE) && entry.needs_copy.load(Ordering::Relaxed);
        if !shadows && let Some(object) = NonNull::new(object.load(Ordering::Relaxed)) {
            // SAFETY: the entry holds a reference on its object, which the
            // map's lock keeps, or it is the kernel object, whose own
            // reference never drops; this handle borrows it and never drops
            // it.
            let held = ManuallyDrop::new(unsafe { Reference::from_raw(object) });
            // vm_fault's reference: the entry's keeps the object only while
            // the map is locked.
            let object = Reference::clone(&held);
            // The object's token, under the map's lock.
            // SAFETY: the lookup's reference keeps the object while its guard
            // lives.
            let state = unsafe { Reference::as_ptr(&object).as_ref() }.lock();
            let offset = Offset::from(offset.load(Ordering::Relaxed));
            let lookup = entry.found(address, object, state, offset, contents.timestamp);
            drop(contents);
            return Ok(lookup);
        }
        drop(contents);
        // The read lock is lost: the lookup runs again under the write lock,
        // which moves the map's version on, shadows the object if a write
        // still needs a copy, and makes it if the entry still has none.
        let contents = self.lock();
        let entry = contents.find(address).ok_or(Error::InvalidAddress)?;
        let (slot, offset) = entry.checked_backing(access);
        if access.contains(Protection::WRITE) && entry.needs_copy.load(Ordering::Relaxed) {
            // A copy shares the object, so there is one.
            let source = NonNull::new(slot.load(Ordering::Relaxed)).unwrap();
            // SAFETY: an entry that needs a copy shares an object `copy_in`
            // took from an unwired entry (it refuses a wired one), so not the
            // kernel object: it holds the object's reference, as `into_raw`
            // gave it up. That reference moves into the shadow.
            let source = unsafe { Reference::from_raw(source) };
            let shadow = object::shadow(
                source,
                Offset::from(offset.load(Ordering::Relaxed)),
                entry.end() - entry.start(),
            );
            // The entry holds the shadow's first reference, from its start.
            slot.store(Reference::into_raw(shadow).as_ptr(), Ordering::Relaxed);
            offset.store(0, Ordering::Relaxed);
            entry.needs_copy.store(false, Ordering::Relaxed);
        }
        let object = NonNull::new(slot.load(Ordering::Relaxed)).map_or_else(
            || {
                let object = object::allocate(entry.end() - entry.start());
                let lookup = Reference::clone(&object);
                // The entry holds the first reference.
                slot.store(Reference::into_raw(object).as_ptr(), Ordering::Relaxed);
                offset.store(0, Ordering::Relaxed);
                lookup
            },
            // SAFETY: as above.
            |object| Reference::clone(&ManuallyDrop::new(unsafe { Reference::from_raw(object) })),
        );
        // The object's token, under the map's lock.
        // SAFETY: as above.
        let state = unsafe { Reference::as_ptr(&object).as_ref() }.lock();
        let offset = Offset::from(offset.load(Ordering::Relaxed));
        let lookup = entry.found(address, object, state, offset, contents.timestamp);
        drop(contents);
        Ok(lookup)
    }

    /// Checks that an entry holds `address` and allows `access`, as gsync's
    /// `probe_address` does through `vm_map_lookup`, under the read lock,
    /// dropped before it returns; it makes no object and locks none.
    ///
    /// # Errors
    ///
    /// `InvalidAddress` if no entry holds `address` or it doesn't allow
    /// `access`: `probe_address` fails, which gsync gives as
    /// `KERN_INVALID_ADDRESS`.
    ///
    /// # Panics
    ///
    /// For a submap: not yet.
    pub fn probe(&self, address: VirtualAddress, access: Protection) -> Result<(), Error> {
        let contents = self.contents.read();
        let entry = contents.find(address).ok_or(Error::InvalidAddress)?;
        if let Backing::Submap(_) = entry.backing {
            panic!("vm_map_lookup: a submap, not yet");
        }
        let allowed = entry.protection().contains(access);
        drop(contents);
        if allowed {
            Ok(())
        } else {
            Err(Error::InvalidAddress)
        }
    }

    /// Takes its read lock if it is still at `version`, as a lookup found
    /// it; it stays locked until what is returned drops.
    pub fn verify(&self, version: u32) -> Option<impl Sized + '_> {
        let contents = self.contents.read();
        (contents.timestamp == version).then_some(contents)
    }

    /// Takes its write lock, moving its version on.
    pub(super) fn lock(&self) -> impl DerefMut<Target = Contents> + '_ {
        let mut contents = self.contents.write();
        contents.timestamp = contents.timestamp.wrapping_add(1);
        contents
    }
}

/// What `Map::copy_in` does with the range it copies.
#[derive(Clone, Copy)]
pub enum Source {
    /// Leaves it in the map, sharing its objects with the copy.
    Keep,
    /// Removes it from the map.
    Destroy,
}

/// Whether `Map::pageable` checks its range.
#[derive(Clone, Copy)]
pub enum RangeCheck {
    /// It holds no hole, and each entry allows the access.
    Checked,
    /// It is taken as it is.
    Unchecked,
}

/// Which of its protections `Map::protect` sets on each entry.
#[derive(Clone, Copy)]
pub enum Change {
    /// What it allows now.
    Current,
    /// The most it may allow, which also cuts what it allows now.
    Maximum,
}

/// Where `Map::enter` puts what it maps.
#[derive(Clone, Copy)]
pub enum Placement {
    /// From the address given.
    Fixed(VirtualAddress),
    /// Wherever it fits.
    Anywhere,
}

/// What an entry `Map::enter_anywhere` makes is for.
#[derive(Clone, Copy)]
pub enum Usage {
    /// Memory, wired throughout.
    Memory,
    /// A kernel stack, above a guard page that is never wired.
    KernelStack,
}

/// What `Map::lookup` finds for an address; `object` must outlive `state`.
pub struct Lookup {
    /// The object's state, locked; it drops before `object`, whose
    /// reference keeps it.
    pub(super) state: TokenGuard<'static, ObjectState>,
    /// The object behind it, with a reference of the lookup's.
    pub(super) object: Reference<Object>,
    /// Where the address is in the object.
    pub(super) offset: Offset,
    /// What the entry allows, less writing while it needs a copy.
    pub(super) protection: Protection,
    /// Whether the entry is wired.
    pub(super) wired: bool,
    /// The map's version at the lookup.
    pub(super) version: u32,
}

// SAFETY: its count is its own, and `free` frees nothing yet.
unsafe impl Counted for Map {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `map`, as `vm_map_deallocate` does at zero.
    ///
    /// # Panics
    ///
    /// Always: not yet.
    unsafe fn free(_map: NonNull<Self>) {
        panic!("vm_map_deallocate: the last reference, not yet");
    }
}

impl Contents {
    /// Checks that `size` more bytes keep the map within its size limit, not
    /// counting what may never be accessed; the kernel's maps have none.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if the size overflows, and `NoSpace` if it passes
    /// the limit.
    fn enforce_limit(&self, size: usize) -> Result<(), Error> {
        let new_size = (self.size - self.size_none)
            .checked_add(size)
            .ok_or(Error::InvalidArgument)?;
        if new_size > self.size_limit {
            return Err(Error::NoSpace);
        }
        Ok(())
    }

    /// The entry that holds `address`.
    fn find(&self, address: VirtualAddress) -> Option<&Entry> {
        self.entries
            .upper_bound(Bound::Included(&address))
            .get()
            .filter(|entry| address < entry.end())
    }

    /// Where `size` bytes fit at a multiple of `align`, a multiple of the
    /// page size, and the entry they would follow: after the smallest gap
    /// that holds them however far aligning skips, the first inserted among
    /// equals, or else from the map's `start`, if the leading gap holds
    /// them.
    fn find_space(
        &self,
        start: VirtualAddress,
        size: usize,
        align: usize,
    ) -> Result<(Option<UnsafeRef<Entry>>, VirtualAddress), Error> {
        // Aligning a page boundary skips at most `align - PAGE_SIZE`.
        let fit = self
            .gaps
            .lower_bound(Bound::Included(&(size + align - PAGE_SIZE, 0)));
        let aligned = start.next_multiple_of(align);
        match fit.get() {
            Some(entry) => Ok((fit.clone_pointer(), entry.end().next_multiple_of(align))),
            None if self.leading_gap >= aligned - start + size => Ok((None, aligned)),
            None => Err(Error::NoSpace),
        }
    }

    /// Where `size` bytes fit anywhere in a map of `bounds`: at its start
    /// while it is empty, else right after `first_free`, else after the
    /// smallest gap that holds them, the last inserted among equals.
    ///
    /// # Panics
    ///
    /// If no free space holds them: not yet.
    fn find_anywhere(&self, bounds: &Range<VirtualAddress>, size: usize) -> VirtualAddress {
        if self.entries.is_empty() {
            assert!(
                bounds.end - bounds.start >= size,
                "vm_map_find_entry_anywhere: no more room, not yet"
            );
            return bounds.start;
        }
        if let Some(entry) = self
            .first_free
            .as_ref()
            .filter(|entry| entry.gap.load(Ordering::Relaxed) >= size)
        {
            return entry.end();
        }
        let gap = self
            .gaps
            .lower_bound(Bound::Included(&(size, 0)))
            .get()
            .unwrap_or_else(|| panic!("vm_map_find_entry_anywhere: no more room, not yet"))
            .gap
            .load(Ordering::Relaxed);
        // The last inserted of that size, as GNU Mach's gap list.
        self.gaps
            .upper_bound(Bound::Included(&(gap, u64::MAX)))
            .get()
            .unwrap()
            .end()
    }

    /// Links `entry` after `previous`, or first, keeping the gaps and size
    /// up to date; it fits between `previous` and the entry after it, or
    /// the map's `start` and end. Returns it, as linked.
    fn link(
        &mut self,
        start: VirtualAddress,
        previous: Option<UnsafeRef<Entry>>,
        entry: CacheBox<Entry>,
    ) -> UnsafeRef<Entry> {
        let (range, gap) = (
            entry.start()..entry.end(),
            entry.gap.load(Ordering::Relaxed),
        );
        // The entry now ends the gap after the one before it, or the leading
        // gap.
        match previous {
            Some(previous) => {
                if previous.gap_link.is_linked() {
                    let key = (
                        previous.gap.load(Ordering::Relaxed),
                        previous.gap_sequence.load(Ordering::Relaxed),
                    );
                    self.gaps.find_mut(&key).remove();
                }
                let previous_gap = range.start - previous.end();
                previous.gap.store(previous_gap, Ordering::Relaxed);
                if previous_gap != 0 {
                    previous
                        .gap_sequence
                        .store(self.gap_inserts, Ordering::Relaxed);
                    self.gap_inserts += 1;
                    self.gaps.insert(previous);
                }
            }
            None => self.leading_gap = range.start - start,
        }
        let entry = CacheBox::into_raw(entry).as_ptr();
        // SAFETY: the trees take the entry, once each, until `unlink` gives
        // it back.
        self.entries.insert(unsafe { UnsafeRef::from_raw(entry) });
        if gap != 0 {
            // SAFETY: as above.
            let entry = unsafe { UnsafeRef::from_raw(entry) };
            entry
                .gap_sequence
                .store(self.gap_inserts, Ordering::Relaxed);
            self.gap_inserts += 1;
            self.gaps.insert(entry);
        }
        self.size += range.end - range.start;
        // SAFETY: as above; `Map::delete` moves the free space hint off an
        // entry before it goes.
        unsafe { UnsafeRef::from_raw(entry) }
    }

    /// Unlinks `entry` from the map of `bounds`, its write lock held: the
    /// free space after it joins the entry before's, or the leading gap.
    /// Returns it, to go back to its cache.
    ///
    /// # Safety
    ///
    /// `entry` is not the free space hint and not in transition, and the
    /// caller uses no pointer to it afterwards.
    ///
    /// # Panics
    ///
    /// If `entry` is not in the map.
    unsafe fn unlink(&mut self, bounds: &Range<VirtualAddress>, entry: &Entry) -> CacheBox<Entry> {
        let start = entry.start();
        let mut cursor = self.entries.find_mut(&start);
        let previous = cursor.peek_prev().clone_pointer();
        let entry = cursor.remove().expect("vm_map_entry_unlink: no entry");
        let next = cursor.get().map_or(bounds.end, Entry::start);
        if entry.gap_link.is_linked() {
            let key = (
                entry.gap.load(Ordering::Relaxed),
                entry.gap_sequence.load(Ordering::Relaxed),
            );
            self.gaps.find_mut(&key).remove();
        }
        match previous {
            Some(previous) => {
                if previous.gap_link.is_linked() {
                    let key = (
                        previous.gap.load(Ordering::Relaxed),
                        previous.gap_sequence.load(Ordering::Relaxed),
                    );
                    self.gaps.find_mut(&key).remove();
                }
                let gap = next - previous.end();
                previous.gap.store(gap, Ordering::Relaxed);
                if gap != 0 {
                    previous
                        .gap_sequence
                        .store(self.gap_inserts, Ordering::Relaxed);
                    self.gap_inserts += 1;
                    self.gaps.insert(previous);
                }
            }
            None => self.leading_gap = next - bounds.start,
        }
        let entry = NonNull::new(UnsafeRef::into_raw(entry)).unwrap();
        // SAFETY: a map's entries come from `CacheBox::into_raw` of
        // `ENTRY_CACHE`'s boxes (`link`, `clip_start`, `clip_end`). Besides
        // its trees, the free space hint and the caller's pointer, an entry is
        // reached only under the map's lock, which the caller holds for
        // writing, or by `Map::pageable_locked` while it is in transition; the
        // caller's contract rules out the others, so out of the trees, it is
        // the box's alone.
        unsafe { CacheBox::from_raw(&ENTRY_CACHE, entry) }
    }

    /// Splits `entry` at `start`, inside it: an entry from `reservation`
    /// takes the part before `start`, with a reference on its object, and
    /// the entry before them has its free space inserted again.
    ///
    /// # Panics
    ///
    /// If `reservation` is used up, or for a submap: not yet.
    fn clip_start(
        &mut self,
        entry: &Entry,
        start: VirtualAddress,
        reservation: &mut EntryReservation,
    ) {
        let Backing::Object { object, offset } = &entry.backing else {
            panic!("_vm_map_clip_start: a submap, not yet");
        };
        // A full copy of it, up to `start`.
        let mut new = reservation.take();
        *new = Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            start: AtomicUsize::new(usize::from(entry.start())),
            end: AtomicUsize::new(usize::from(start)),
            gap: AtomicUsize::new(0),
            gap_sequence: AtomicU64::new(0),
            protection: AtomicU32::new(entry.protection().bits()),
            max_protection: AtomicU32::new(entry.max_protection().bits()),
            inheritance: entry.inheritance,
            backing: Backing::Object {
                object: AtomicPtr::new(object.load(Ordering::Relaxed)),
                offset: AtomicUsize::new(offset.load(Ordering::Relaxed)),
            },
            wire_count: AtomicU32::new(entry.wire_count()),
            wired_access: AtomicU32::new(entry.wired_access().bits()),
            in_transition: AtomicBool::new(entry.in_transition.load(Ordering::Relaxed)),
            needs_copy: AtomicBool::new(entry.needs_copy.load(Ordering::Relaxed)),
            kernel_stack: entry.kernel_stack,
        };
        offset.store(
            offset.load(Ordering::Relaxed) + (start - entry.start()),
            Ordering::Relaxed,
        );
        // Moved first, so the new entry sorts before it.
        entry.start.store(usize::from(start), Ordering::Relaxed);
        // Linked before it: the free space of the entry before is inserted
        // again, as the last of its size.
        let previous = self.entries.find(&start).peek_prev().clone_pointer();
        if let Some(previous) = previous.filter(|previous| previous.gap_link.is_linked()) {
            let key = (
                previous.gap.load(Ordering::Relaxed),
                previous.gap_sequence.load(Ordering::Relaxed),
            );
            let previous = self.gaps.find_mut(&key).remove().unwrap();
            previous
                .gap_sequence
                .store(self.gap_inserts, Ordering::Relaxed);
            self.gap_inserts += 1;
            self.gaps.insert(previous);
        }
        let new = CacheBox::into_raw(new).as_ptr();
        // SAFETY: the tree takes the entry, once, until `unlink` gives it
        // back.
        self.entries.insert(unsafe { UnsafeRef::from_raw(new) });
        if let Some(object) = NonNull::new(object.load(Ordering::Relaxed)) {
            // SAFETY: the entry holds a reference on its object, or it is the
            // kernel object, whose own reference never drops; this handle
            // borrows it and never drops it.
            let held = ManuallyDrop::new(unsafe { Reference::from_raw(object) });
            // Another, held by the new entry.
            Reference::into_raw(Reference::clone(&held));
        }
    }

    /// Splits `entry` at `end`, inside it: an entry from `reservation`
    /// takes the part from `end` on, with the free space after it and a
    /// reference on its object.
    ///
    /// # Panics
    ///
    /// If `reservation` is used up, or for a submap: not yet.
    fn clip_end(&mut self, entry: &Entry, end: VirtualAddress, reservation: &mut EntryReservation) {
        let Backing::Object { object, offset } = &entry.backing else {
            panic!("_vm_map_clip_end: a submap, not yet");
        };
        let gap = entry.gap.load(Ordering::Relaxed);
        // A full copy of it, from `end` on.
        let mut new = reservation.take();
        *new = Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            start: AtomicUsize::new(usize::from(end)),
            end: AtomicUsize::new(usize::from(entry.end())),
            gap: AtomicUsize::new(gap),
            gap_sequence: AtomicU64::new(0),
            protection: AtomicU32::new(entry.protection().bits()),
            max_protection: AtomicU32::new(entry.max_protection().bits()),
            inheritance: entry.inheritance,
            backing: Backing::Object {
                object: AtomicPtr::new(object.load(Ordering::Relaxed)),
                offset: AtomicUsize::new(offset.load(Ordering::Relaxed) + (end - entry.start())),
            },
            wire_count: AtomicU32::new(entry.wire_count()),
            wired_access: AtomicU32::new(entry.wired_access().bits()),
            in_transition: AtomicBool::new(entry.in_transition.load(Ordering::Relaxed)),
            needs_copy: AtomicBool::new(entry.needs_copy.load(Ordering::Relaxed)),
            kernel_stack: entry.kernel_stack,
        };
        entry.end.store(usize::from(end), Ordering::Relaxed);
        // Linked after it: the free space after it moves to the new entry.
        if entry.gap_link.is_linked() {
            let key = (gap, entry.gap_sequence.load(Ordering::Relaxed));
            self.gaps.find_mut(&key).remove();
        }
        entry.gap.store(0, Ordering::Relaxed);
        let new = CacheBox::into_raw(new).as_ptr();
        // SAFETY: the trees take the entry, once each, until `unlink` gives
        // it back.
        self.entries.insert(unsafe { UnsafeRef::from_raw(new) });
        if gap != 0 {
            // SAFETY: as above.
            let new = unsafe { UnsafeRef::from_raw(new) };
            new.gap_sequence.store(self.gap_inserts, Ordering::Relaxed);
            self.gap_inserts += 1;
            self.gaps.insert(new);
        }
        if let Some(object) = NonNull::new(object.load(Ordering::Relaxed)) {
            // SAFETY: the entry holds a reference on its object, or it is the
            // kernel object, whose own reference never drops; this handle
            // borrows it and never drops it.
            let held = ManuallyDrop::new(unsafe { Reference::from_raw(object) });
            // Another, held by the new entry.
            Reference::into_raw(Reference::clone(&held));
        }
    }
}

/// Entries allocated before a map's lock, for one operation to take under
/// it, so none is allocated there; those left go back to their cache when
/// it drops.
struct EntryReservation(ArrayVec<CacheBox<Entry>, 4>);

impl EntryReservation {
    /// A full one.
    ///
    /// # Panics
    ///
    /// If an entry can't be allocated.
    fn new() -> Self {
        let mut entries = ArrayVec::new();
        while !entries.is_full() {
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
            entries.push(entry);
        }
        Self(entries)
    }

    /// One of its entries, to be filled in.
    ///
    /// # Panics
    ///
    /// If none is left: more than 4 entries in one operation, not yet.
    fn take(&mut self) -> CacheBox<Entry> {
        self.0
            .pop()
            .expect("vm_map: more than 4 entries in one operation, not yet")
    }
}

/// A range of a map and what it maps; so far only what an entry of an
/// object, wired or not, or of a submap sets.
pub struct Entry {
    /// Its place in its map's tree.
    tree_link: RBTreeAtomicLink,
    /// Its place in its map's gap tree, while free space follows it.
    gap_link: RBTreeAtomicLink,
    /// Where it starts, as a `VirtualAddress`; changed only under the map's
    /// write lock.
    start: AtomicUsize,
    /// Where it ends, past its last address, as a `VirtualAddress`; changed
    /// only under the map's write lock.
    end: AtomicUsize,
    /// The free space after it, up to the next entry or the map's end;
    /// changed only under the map's write lock.
    gap: AtomicUsize,
    /// Its place among equal gaps: its map's `gap_inserts` when its gap was
    /// last inserted; changed only under the map's write lock.
    gap_sequence: AtomicU64,
    /// What it allows now, as `Protection`'s bits; changed only under the
    /// map's write lock.
    protection: AtomicU32,
    /// The most it may ever allow, as `Protection`'s bits; changed only
    /// under the map's write lock.
    max_protection: AtomicU32,
    /// What a child task gets of it.
    inheritance: Inheritance,
    /// What backs it.
    pub(super) backing: Backing,
    /// How many times it is wired: its pages stay resident and mapped;
    /// changed only under the map's write lock.
    wire_count: AtomicU32,
    /// What it allows while wired, as `Protection`'s bits; changed only
    /// under the map's write lock.
    wired_access: AtomicU32,
    /// Whether a wiring has it while the map is unlocked, which keeps it
    /// from merging or going; changed only under the map's write lock.
    in_transition: AtomicBool,
    /// Whether a copy shares its object, which is then shadowed before it
    /// is written; changed only under the map's write lock.
    needs_copy: AtomicBool,
    /// Whether it is a kernel stack, whose first page, its guard, is never
    /// wired; GNU Mach's stacks have no guard.
    pub(super) kernel_stack: bool,
}

impl Entry {
    /// Where it starts.
    pub(super) fn start(&self) -> VirtualAddress {
        VirtualAddress::from(self.start.load(Ordering::Relaxed))
    }

    /// Where it ends, past its last address.
    pub(super) fn end(&self) -> VirtualAddress {
        VirtualAddress::from(self.end.load(Ordering::Relaxed))
    }

    /// How many times it is wired.
    fn wire_count(&self) -> u32 {
        self.wire_count.load(Ordering::Relaxed)
    }

    /// What it allows while wired.
    fn wired_access(&self) -> Protection {
        Protection::from_bits_retain(self.wired_access.load(Ordering::Relaxed))
    }

    /// What it allows now.
    pub(super) fn protection(&self) -> Protection {
        Protection::from_bits_retain(self.protection.load(Ordering::Relaxed))
    }

    /// The most it may ever allow.
    fn max_protection(&self) -> Protection {
        Protection::from_bits_retain(self.max_protection.load(Ordering::Relaxed))
    }

    /// Its object, from an offset, once `vm_map_lookup`'s checks find a fault
    /// asking for `access` may be resolved in it.
    ///
    /// # Panics
    ///
    /// If it is a submap or doesn't allow `access`: not yet.
    fn checked_backing(&self, access: Protection) -> (&AtomicPtr<Object>, &AtomicUsize) {
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
    fn found(
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
}

/// Merges the entry at `cursor` into the one before it, if they meet and map
/// alike.
///
/// # Panics
///
/// If they would merge: not yet.
fn coalesce(cursor: &Cursor<'_, EntryAdapter>) {
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
    EntryAdapter = UnsafeRef<Entry>: Entry { tree_link => RBTreeAtomicLink }
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
    GapAdapter = UnsafeRef<Entry>: Entry { gap_link => RBTreeAtomicLink }
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

/// Memory copied out of a map while in transit, such as out-of-line data; so
/// far only its kind, range and entries.
pub struct MapCopy {
    /// How it holds the memory.
    kind: CopyKind,
    /// Where the memory was, unrounded.
    offset: VirtualAddress,
    /// Its size, in bytes, unrounded.
    size: usize,
    /// For `CopyKind::Entries`, its entries, by their old address; their gap
    /// links are unused.
    entries: RBTree<EntryAdapter>,
}

impl Drop for MapCopy {
    fn drop(&mut self) {
        assert!(
            self.entries.is_empty(),
            "vm_map_copy_discard: a copy holding entries, not yet"
        );
    }
}

/// How a copy holds its memory.
#[expect(
    dead_code,
    reason = "vm_map_copyin_object and the page-list copy-in make them"
)]
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
pub static COPY_CACHE: Cache<MapCopy> = Cache::new("vm_map_copy");

/// Sets up address maps, on the boot CPU.
pub fn init() {
    MAP_CACHE.register();
    ENTRY_CACHE.register();
    COPY_CACHE.register();
}
