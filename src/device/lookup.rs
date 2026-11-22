// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Device lookup, by name, number or port; so far by name and port.

use core::mem::ManuallyDrop;
use core::ptr::NonNull;

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::pager::PagerAdapter;
use super::{Device, DeviceState, Kind, State};
use crate::reference::{Count, Reference};
use crate::slab::{Cache, CacheBox};
use crate::sync::Spinlock;

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    DeviceAdapter = UnsafeRef<Device>: Device { link => LinkedListAtomicLink }
);

/// The devices made so far, by kind and unit; never held while allocating.
static LOOKUP: Spinlock<LinkedList<DeviceAdapter>> =
    Spinlock::new(LinkedList::new(DeviceAdapter::NEW));

/// The cache devices come from.
static CACHE: Cache<Device> = Cache::new("mach_device");

/// Sets up device lookup, on the boot CPU.
pub(super) fn init() {
    CACHE.register();
}

/// The device `name` names, with a reference, made if there is none yet,
/// as `device_lookup` does.
///
/// # Panics
///
/// If no device can be allocated. As `by_name`.
pub(super) fn find(name: &[u8]) -> Reference<Device> {
    let (kind, unit) = by_name(name);
    let mut new = None;
    loop {
        let mut list = LOOKUP.lock();
        let mut cursor = list.front();
        while cursor
            .get()
            .is_some_and(|device| device.kind != kind || device.unit != unit)
        {
            cursor.move_next();
        }
        if let Some(found) = cursor.clone_pointer() {
            let found = NonNull::new(UnsafeRef::into_raw(found)).unwrap();
            // SAFETY: a listed device stays allocated: its free panics, so
            // nothing unlinks it. The handle borrows it and never drops.
            let found = ManuallyDrop::new(unsafe { Reference::from_raw(found) });
            let device = Reference::clone(&found);
            drop(list);
            // One made meanwhile goes back to the cache.
            drop(new);
            return device;
        }
        if let Some(new) = new.take() {
            let device = CacheBox::into_raw(new);
            // SAFETY: the box's object, given up, stays allocated while listed.
            list.push_back(unsafe { UnsafeRef::from_raw(device.as_ptr()) });
            drop(list);
            // SAFETY: the box's one reference, given up above.
            return unsafe { Reference::from_raw(device) };
        }
        drop(list);
        new = Some(
            CACHE
                .allocate(Device {
                    references: Count::new(1),
                    kind,
                    unit,
                    link: LinkedListAtomicLink::new(),
                    state: Spinlock::new(DeviceState {
                        state: State::Init,
                        open_count: 0,
                        port: None,
                        pagers: LinkedList::new(PagerAdapter::NEW),
                    }),
                })
                .unwrap_or_else(|error| panic!("device_lookup: {error:?}")),
        );
    }
}

/// The kind and unit `name` names, as `dev_name_lookup` does: the kind's
/// name, then the unit's digits.
///
/// # Panics
///
/// If no kind has that name: not yet.
fn by_name(name: &[u8]) -> (Kind, u32) {
    let digits = name
        .iter()
        .position(u8::is_ascii_digit)
        .unwrap_or(name.len());
    let (prefix, rest) = name.split_at(digits);
    let unit = rest
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .fold(0_u32, |unit, digit| {
            unit.wrapping_mul(10).wrapping_add(u32::from(digit - b'0'))
        });
    let kind = Kind::ALL
        .into_iter()
        .find(|kind| kind.name() == prefix)
        .unwrap_or_else(|| panic!("dev_name_lookup: dev_indirect_search, not yet"));
    (kind, unit)
}
