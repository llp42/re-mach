// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ops::{DerefMut, Range};
use core::sync::atomic::Ordering;

use intrusive_collections::Bound;

use super::change::{Change, RangeCheck};
use super::contents::Contents;
use super::entry::{Backing, Entry, EntryReservation, coalesce};
use super::map::Map;
use crate::error::Error;
use crate::reference::Reference;
use crate::vm::VirtualAddress;
use crate::vm::fault;
use crate::vm::object;
use crate::vm::phys_map::MachinePhysMap as _;
use crate::vm::{Protection, Wire};

impl Map {
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
                    let new = object::init::allocate(entry.end() - entry.start());
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
}
