// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::ManuallyDrop;
use core::ops::Range;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

use intrusive_collections::{Bound, RBTree, RBTreeAtomicLink, UnsafeRef};

use super::entry::{Backing, ENTRY_CACHE, Entry, EntryAdapter, EntryReservation, GapAdapter};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::reference::Reference;
use crate::slab::CacheBox;
use crate::vm::VirtualAddress;

/// What a map's lock guards.
pub(in crate::vm) struct Contents {
    /// Its entries, by address.
    pub(super) entries: RBTree<EntryAdapter>,
    /// The entries free space follows, by its size, then by when it was
    /// inserted.
    pub(super) gaps: RBTree<GapAdapter>,
    /// How many gaps it has inserted; each insert stamps its entry with the
    /// count before it.
    pub(super) gap_inserts: u64,
    /// The free space before the first entry; all of the map while it is
    /// empty.
    pub(super) leading_gap: usize,
    /// The entry whose gap a search for free space anywhere tries first;
    /// none to skip it.
    pub(super) first_free: Option<UnsafeRef<Entry>>,
    /// How much it maps, in bytes.
    pub(super) size: usize,
    /// How much of it is wired, in bytes.
    pub(super) size_wired: usize,
    /// How much of it may never be accessed, in bytes.
    pub(super) size_none: usize,
    /// Whether what it maps from now on is wired.
    pub(super) wiring_required: bool,
    /// The most it may map, in bytes.
    pub(in crate::vm) size_limit: usize,
    /// The most `size_limit` may be raised to.
    pub(in crate::vm) max_size_limit: usize,
    /// Its version, moved on by every write lock.
    pub(super) timestamp: u32,
}

impl Contents {
    /// Checks that `size` more bytes keep the map within its size limit, not
    /// counting what may never be accessed; the kernel's maps have none.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if the size overflows, and `NoSpace` if it passes
    /// the limit.
    pub(super) fn enforce_limit(&self, size: usize) -> Result<(), Error> {
        let new_size = (self.size - self.size_none)
            .checked_add(size)
            .ok_or(Error::InvalidArgument)?;
        if new_size > self.size_limit {
            return Err(Error::NoSpace);
        }
        Ok(())
    }

    /// Takes the size limits of `source`, as `vm_map_copy_limits` does.
    pub(super) const fn copy_limits(&mut self, source: &Self) {
        self.size_limit = source.size_limit;
        self.max_size_limit = source.max_size_limit;
    }

    /// The entry that holds `address`.
    pub(super) fn find(&self, address: VirtualAddress) -> Option<&Entry> {
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
    pub(super) fn find_space(
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
    pub(super) fn find_anywhere(
        &self,
        bounds: &Range<VirtualAddress>,
        size: usize,
    ) -> VirtualAddress {
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
    pub(super) fn link(
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
    pub(super) unsafe fn unlink(
        &mut self,
        bounds: &Range<VirtualAddress>,
        entry: &Entry,
    ) -> CacheBox<Entry> {
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
    pub(super) fn clip_start(
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
    pub(super) fn clip_end(
        &mut self,
        entry: &Entry,
        end: VirtualAddress,
        reservation: &mut EntryReservation,
    ) {
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
