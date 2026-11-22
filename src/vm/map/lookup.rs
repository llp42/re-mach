// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::ManuallyDrop;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use super::entry::Backing;
use super::map::Map;
use crate::error::Error;
use crate::reference::Reference;
use crate::sync::TokenGuard;
use crate::vm::object::{self, object::Object, offset::Offset, state::ObjectState};
use crate::vm::{Protection, VirtualAddress};

/// What `Map::lookup` finds for an address; `object` must outlive `state`.
pub struct Lookup {
    /// The object's state, locked; it drops before `object`, whose
    /// reference keeps it.
    pub(in crate::vm) state: TokenGuard<'static, ObjectState>,
    /// The object behind it, with a reference of the lookup's.
    pub(in crate::vm) object: Reference<Object>,
    /// Where the address is in the object.
    pub(in crate::vm) offset: Offset,
    /// What the entry allows, less writing while it needs a copy.
    pub(in crate::vm) protection: Protection,
    /// Whether the entry is wired.
    pub(in crate::vm) wired: bool,
    /// The map's version at the lookup.
    pub(in crate::vm) version: u32,
}

impl Map {
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
            let shadow = object::shadow::shadow(
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
                let object = object::init::allocate(entry.end() - entry.start());
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
}
