// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::sync::atomic::Ordering;

use super::entry::{Backing, EntryReservation};
use super::map::Map;
use crate::arch::PhysMap;
use crate::error::Error;
use crate::slab::CacheBox;
use crate::vm::Inheritance;
use crate::vm::phys_map::MachinePhysMap as _;

impl Map {
    /// A new map of the same bounds over a new physical map, holding a copy
    /// of each entry as its inheritance says, as `vm_map_fork` makes it: a
    /// copied entry shares its object with the map until either writes, and
    /// the map's mappings there stop allowing writing.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the physical map can't be allocated.
    ///
    /// # Panics
    ///
    /// For a submap, as GNU Mach. If the map or an entry can't be allocated,
    /// or an entry is not inherited, is wired or holds an object that is not
    /// temporary: not yet. As `MachinePhysMap::protect`.
    pub fn fork(&self) -> Result<CacheBox<Self>, Error> {
        let phys_map = PhysMap::create()?;
        // Allocated before the lock; the bounds never change.
        let mut new = Self::create(phys_map, self.bounds.clone())
            .unwrap_or_else(|error| panic!("vm_map_fork: {error:?}, pmap_destroy not yet"));
        // An entry for each, and a few spare, taken before the write lock;
        // again if the map grew in between.
        let mut reservation = EntryReservation::new();
        let guard = loop {
            let count = self.contents.read().entries.iter().count();
            reservation.reserve(count + 4);
            let guard = self.lock();
            if guard.entries.iter().count() <= reservation.count {
                break guard;
            }
            drop(guard);
        };
        let contents = &*guard;
        let new_contents = new.contents.get_mut();
        let mut size_none = 0;
        let mut previous = None;
        for entry in &contents.entries {
            assert!(
                matches!(entry.backing, Backing::Object { .. }),
                "vm_map_fork: encountered a submap"
            );
            match entry.inheritance {
                Inheritance::Copy => {}
                Inheritance::None => panic!("vm_map_fork: VM_INHERIT_NONE, not yet"),
            }
            assert!(
                entry.wire_count() == 0,
                "vm_map_fork: a wired entry, vm_map_copyin, not yet"
            );
            let new_entry = entry.copy(&mut reservation);
            assert!(
                self.copy_temporary(entry, &new_entry),
                "vm_map_fork: vm_map_copyin, not yet"
            );
            // Linked last: the free space after it reaches the map's end.
            new_entry
                .gap
                .store(self.bounds.end - new_entry.end(), Ordering::Relaxed);
            if entry.max_protection().is_empty() {
                size_none += entry.end() - entry.start();
            }
            previous = Some(new_contents.link(self.bounds.start, previous, new_entry));
        }
        // `link` added each entry's size.
        new_contents.size_none = size_none;
        new_contents.copy_limits(contents);
        drop(guard);
        Ok(new)
    }
}
