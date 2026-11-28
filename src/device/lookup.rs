// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Device lookup, by name, number or port; so far by name and port.

use core::mem::ManuallyDrop;
use core::ptr::NonNull;

use tailq::{Link, TailQueue};

use super::{Device, DeviceState, Kind, State};
use crate::reference::{Count, Reference};
use crate::slab::{Cache, CacheBox};
use crate::sync::Spinlock;

tailq::adapter!(DeviceAdapter = Device { link });

/// The devices made so far, by kind and unit; never held while allocating.
static LOOKUP: Spinlock<TailQueue<'static, DeviceAdapter>> = Spinlock::new(TailQueue::new());

/// The cache devices come from.
static CACHE: Cache<Device> = Cache::new("mach_device");

/// Sets up device lookup, on the boot CPU.
pub(super) fn init() {
    CACHE.register();
}

/// The device `name` names, with a reference, made if there is none yet,
/// as `device_lookup` does; none if no kind has its name.
///
/// # Panics
///
/// If no device can be allocated.
pub(super) fn find(name: &[u8]) -> Option<Reference<Device>> {
    let (kind, unit) = by_name(name)?;
    let mut new = None;
    loop {
        let mut list = LOOKUP.lock();
        let mut cursor = list.cursor_front();
        while cursor
            .current()
            .is_some_and(|device| device.kind != kind || device.unit != unit)
        {
            cursor.move_next();
        }
        if let Some(found) = cursor.current_ptr() {
            // SAFETY: a listed device stays allocated: its free panics, so
            // nothing unlinks it. The handle borrows it and never drops.
            let found = ManuallyDrop::new(unsafe { Reference::from_raw(found) });
            let device = Reference::clone(&found);
            drop(list);
            // One made meanwhile goes back to the cache.
            drop(new);
            return Some(device);
        }
        if let Some(new) = new.take() {
            let device = CacheBox::into_raw(new);
            // SAFETY: the box's object, given up, stays allocated while
            // listed, and only this list writes its link, under the lock.
            unsafe { list.push_back_ptr(device) };
            drop(list);
            // SAFETY: the box's one reference, given up above.
            return Some(unsafe { Reference::from_raw(device) });
        }
        drop(list);
        new = Some(
            CACHE
                .allocate(Device {
                    references: Count::new(1),
                    kind,
                    unit,
                    link: Link::new(),
                    state: Spinlock::new(DeviceState {
                        state: State::Init,
                        open_count: 0,
                        port: None,
                        pagers: TailQueue::new(),
                    }),
                })
                .unwrap_or_else(|error| panic!("device_lookup: {error:?}")),
        );
    }
}

/// Frees `device`, as `mach_device_deallocate` does at zero: it leaves the
/// list, then goes back to its cache.
///
/// # Safety
///
/// Nothing uses it after.
pub(super) unsafe fn free(device: NonNull<Device>) {
    {
        let mut list = LOOKUP.lock();
        // SAFETY: a device at its last reference was listed, and its link
        // still points into the list.
        unsafe { list.remove_ptr(device) };
    }
    // SAFETY: every device is a `CACHE` box, given up by
    // `CacheBox::into_raw`; nothing uses this one after.
    drop(unsafe { CacheBox::from_raw(&CACHE, device) });
}

/// The kind and unit `name` names, as `dev_name_lookup` does: the kind's
/// name, then the unit's digits; none if no kind has that name.
fn by_name(name: &[u8]) -> Option<(Kind, u32)> {
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
    let kind = Kind::ALL.into_iter().find(|kind| kind.name() == prefix)?;
    Some((kind, unit))
}
