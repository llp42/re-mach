// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! What child tasks get of a map's ranges.

use core::ops::Range;
use core::sync::atomic::Ordering;

use intrusive_collections::Bound;

use super::entry::EntryReservation;
use super::map::Map;
use crate::error::Error;
use crate::vm::{Inheritance, VirtualAddress};

impl Map {
    /// Sets what child tasks get of `range`, as `vm_map_inherit` does.
    ///
    /// # Errors
    ///
    /// `NoSpace` if no entry holds the range's start.
    ///
    /// # Panics
    ///
    /// As `Contents::clip_start` and `Contents::clip_end`.
    pub fn inherit(
        &self,
        range: Range<VirtualAddress>,
        inheritance: Inheritance,
    ) -> Result<(), Error> {
        // Taken before the lock, for the splits.
        let mut reservation = EntryReservation::new();
        let mut contents = self.lock();
        let end = range.end.min(self.bounds.end);
        let start = range.start.max(self.bounds.start).min(end);
        let Some(first) = contents
            .entries
            .upper_bound(Bound::Included(&start))
            .clone_pointer()
            .filter(|entry| start < entry.end())
        else {
            return Err(Error::NoSpace);
        };
        if start > first.start() {
            contents.clip_start(&first, start, &mut reservation);
        }
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
            entry
                .inheritance
                .store(inheritance.number(), Ordering::Relaxed);
            address = entry.end();
        }
        drop(contents);
        Ok(())
    }
}
