// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ops::Range;
use core::ptr::{self, NonNull};
use core::sync::atomic::Ordering;

use intrusive_collections::Bound;

use super::contents::Contents;
use super::entry::{Backing, EntryReservation};
use super::map::Map;
use crate::reference::Reference;
use crate::vm::VirtualAddress;
use crate::vm::fault;
use crate::vm::object;
use crate::vm::object::state::Flags;
use crate::vm::phys_map::MachinePhysMap as _;

impl Map {
    /// Removes the entries of `range` from the map, `contents` its contents
    /// under its write lock, unwiring them, and their pages from its physical
    /// map, splitting entries at the range's start and end with ones from
    /// `reservation`. The kernel object's pages there are freed.
    ///
    /// # Panics
    ///
    /// If an entry there is in transition or a submap: not yet. As
    /// `Contents::clip_start`, `Contents::clip_end`, `fault::unwire` and
    /// `MachinePhysMap::remove`; at an object's last reference, as its free.
    pub(super) fn delete(
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
                // Unwired, unmapped on every CPU, then freed: no CPU reaches a
                // freed page.
                if entry.wire_count() != 0 {
                    // vm_map_entry_reset_wired.
                    contents.size_wired -= entry.end() - entry.start();
                    entry.wire_count.store(0, Ordering::Relaxed);
                    fault::unwire(self, &entry);
                }
                // Entries are shared only by vm_map_fork, not yet. The kernel
                // object's pages too leave by range (GNU Mach: page by page).
                self.phys_map
                    .remove(usize::from(entry.start())..usize::from(entry.end()));
                // The kernel object's pages go now; another's with no pager
                // and no other reference.
                let mut state = object.lock();
                if ptr::eq(object, &raw const object::object::KERNEL_OBJECT)
                    || (!state.flags.contains(Flags::PAGER_CREATED)
                        && object.references() == 1
                        && state.paging_in_progress == 0)
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
}
