// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The region an address is in, as `vm_region` finds it.

use core::mem::ManuallyDrop;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use intrusive_collections::Bound;

use super::entry::Backing;
use super::map::Map;
use crate::error::Error;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::vm::object::offset::Offset;
use crate::vm::{Inheritance, Protection, VirtualAddress};

/// A range of a map and what it maps, as `vm_region` finds it.
pub struct Region {
    /// Where it starts.
    pub(crate) start: VirtualAddress,
    /// Where it ends, past its last address.
    pub(crate) end: VirtualAddress,
    /// What it allows now.
    pub(crate) protection: Protection,
    /// The most it may ever allow.
    pub(crate) max_protection: Protection,
    /// What a child task gets of it.
    pub(crate) inheritance: Inheritance,
    /// A send right for its object's name port, if it has one.
    pub(crate) object_name: Option<Reference<Port>>,
    /// Where it starts in the object.
    pub(crate) offset: Offset,
    /// A send right for the object's pager's port, if it has one.
    pub(crate) pager: Option<Reference<Port>>,
}

impl Map {
    /// Finds the region `address` is in: where it starts and ends, the most
    /// it may allow, its object from `offset`, and a send right for the
    /// object's pager's port if it has one.
    ///
    /// # Errors
    ///
    /// `NoSpace` if no entry holds `address`.
    ///
    /// # Panics
    ///
    /// For a submap, or an entry with no object: not yet.
    pub fn region(&self, address: VirtualAddress) -> Result<Region, Error> {
        let (start, end, protection, max_protection, inheritance, offset, object) = {
            let contents = self.contents.read();
            let entry = contents
                .find(address)
                .or_else(|| {
                    contents
                        .entries
                        .lower_bound(Bound::Excluded(&address))
                        .get()
                })
                .ok_or(Error::NoSpace)?;
            let Backing::Object { object, offset } = &entry.backing else {
                panic!("vm_region: a submap, not yet");
            };
            let Some(object) = NonNull::new(object.load(Ordering::Relaxed)) else {
                panic!("vm_region: an entry with no object, not yet");
            };
            // SAFETY: the entry holds the object's reference, which the
            // map's lock keeps, or it is the kernel object, whose own
            // reference never drops; this handle borrows it and never drops
            // it.
            let held = ManuallyDrop::new(unsafe { Reference::from_raw(object) });
            let values = (
                entry.start(),
                entry.end(),
                entry.protection(),
                entry.max_protection(),
                Inheritance::from_number(u32::from(entry.inheritance.load(Ordering::Relaxed)))
                    .expect("vm_region: a bad inheritance"),
                Offset::from(offset.load(Ordering::Relaxed)),
                Reference::clone(&held),
            );
            drop(contents);
            values
        };
        Ok(Region {
            start,
            end,
            protection,
            max_protection,
            inheritance,
            object_name: object.name_port(),
            offset,
            pager: object.pager_port(),
        })
    }
}
