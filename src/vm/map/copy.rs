// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::ManuallyDrop;
use core::ops::Range;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use intrusive_collections::{Bound, RBTree, UnsafeRef};

use super::change::Source;
use super::entry::{Backing, ENTRY_CACHE, Entry, EntryAdapter, EntryReservation};
use super::map::Map;
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::reference::Reference;
use crate::slab::{Cache, CacheBox};
use crate::vm::Inheritance;
use crate::vm::Protection;
use crate::vm::VirtualAddress;
use crate::vm::object;
use crate::vm::object::Flags;
use crate::vm::phys_map::MachinePhysMap as _;

impl Map {
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
    /// temporary; for `Source::Keep`, if an entry is wired: not yet. As
    /// `Contents::clip_start`, `Contents::clip_end`,
    /// `MachinePhysMap::protect` and `Map::delete`.
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
            let Backing::Object { object, .. } = &entry.backing else {
                panic!("vm_map_copyin: a submap, not yet");
            };
            let object = object.load(Ordering::Relaxed);
            let wired = entry.wire_count() != 0;
            let new = entry.copy(&mut reservation);
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
                assert!(
                    self.copy_temporary(&entry, &new),
                    "vm_map_copyin: vm_object_copy_strategically, not yet"
                );
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
    /// Has `new`, a copy of `entry`, share `entry`'s object until either
    /// writes, as `vm_object_copy_temporary` shares a temporary object:
    /// `new` takes another reference, both need a copy, and the map's
    /// mappings of `entry` stop allowing writing, as
    /// `vm_object_pmap_protect`. Gives whether it could; nothing changes for
    /// an object that is not temporary. With no object, `new` has none
    /// either and needs no copy.
    ///
    /// # Panics
    ///
    /// As `MachinePhysMap::protect`.
    pub(super) fn copy_temporary(&self, entry: &Entry, new: &Entry) -> bool {
        let Backing::Object { object, .. } = &entry.backing else {
            panic!("vm_object_copy_temporary: a submap");
        };
        let Some(object) = NonNull::new(object.load(Ordering::Relaxed)) else {
            new.needs_copy.store(false, Ordering::Relaxed);
            return true;
        };
        // SAFETY: the entry holds a reference on its object, or it is the
        // kernel object, whose own reference never drops; this handle borrows
        // it and never drops it.
        let object = ManuallyDrop::new(unsafe { Reference::from_raw(object) });
        let Some(shared) = object::copy_temporary(&object) else {
            return false;
        };
        // `new` holds it; both entries shadow the object before writing.
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
        true
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

/// The cache copies come from.
pub static COPY_CACHE: Cache<MapCopy> = Cache::new("vm_map_copy");
