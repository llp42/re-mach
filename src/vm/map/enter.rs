// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::ManuallyDrop;
use core::ops::Range;
use core::ptr::{self, NonNull};
use core::sync::atomic::{
    AtomicBool, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use intrusive_collections::{Bound, RBTreeAtomicLink};

use super::change::{Placement, Usage};
use super::entry::{Backing, ENTRY_CACHE, Entry, coalesce};
use super::map::Map;
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::reference::Reference;
use crate::sync::OnceLock;
use crate::vm::object::{self, object::Object, offset::Offset};
use crate::vm::phys_map::MachinePhysMap as _;
use crate::vm::{Inheritance, Protection, VirtualAddress};

impl Map {
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
            inheritance: AtomicU8::new(inheritance.number()),
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
                && neighbour.inheritance.load(Ordering::Relaxed)
                    == entry.inheritance.load(Ordering::Relaxed)
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
            && let Some((joined, object_offset)) = object::shadow::coalesce(
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
            && object::shadow::coalesce(
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
            inheritance: AtomicU8::new(Inheritance::Copy.number()),
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
            inheritance: AtomicU8::new(Inheritance::Copy.number()),
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
}
