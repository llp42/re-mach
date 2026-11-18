// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The device pager, which backs memory objects mapped from devices; so
//! far only its records and their making.

use intrusive_collections::{LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::{Device, Kind};
use crate::ipc::port::{KernelObject, Port};
use crate::reference::Reference;
use crate::slab::{Cache, CacheBox};
use crate::vm::Protection;
use crate::vm::object::Offset;

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub(super) PagerAdapter = UnsafeRef<Pager>: Pager { link => LinkedListAtomicLink }
);

/// The cache pagers come from.
static CACHE: Cache<Pager> = Cache::new("dev_pager");

/// A device's memory from an offset, as one memory object: its device,
/// offset and protection, its port, and its place in its device's pager
/// list.
pub struct Pager {
    /// Its device, which it holds a reference to.
    #[expect(dead_code, reason = "device_map_page reads it")]
    device: Reference<Device>,
    /// Where it starts in its device's memory.
    offset: Offset,
    /// What it allows.
    #[expect(dead_code, reason = "device_map_page reads it")]
    protection: Protection,
    /// Its port, the memory object.
    port: Reference<Port>,
    /// Its place in its device's pager list.
    link: LinkedListAtomicLink,
}

/// Sets up the device pager, on the boot CPU.
pub(super) fn init() {
    CACHE.register();
}

/// A send right for the pager of `device`'s memory from `offset`, made
/// allowing `protection`, as `device_pager_setup` does; no size is kept,
/// as GNU Mach never reads it.
///
/// # Panics
///
/// If the device's kind can't be mapped, it has a pager at `offset`, or
/// no pager or port can be allocated: not yet.
pub(super) fn setup(
    device: &Reference<Device>,
    protection: Protection,
    offset: Offset,
) -> Reference<Port> {
    // d_mmap: the kinds that map.
    assert!(
        matches!(device.kind, Kind::Time),
        "device_pager_setup: D_INVALID_OPERATION, not yet"
    );
    // dev_device_hash_lookup.
    assert!(
        device
            .state
            .lock()
            .pagers
            .iter()
            .all(|pager| pager.offset != offset),
        "device_pager_setup: an existing pager, not yet"
    );
    let port = Reference::from(
        Port::allocate_kernel()
            .unwrap_or_else(|_| panic!("device_pager_setup: KERN_RESOURCE_SHORTAGE, not yet")),
    );
    let pager = CACHE
        .allocate(Pager {
            // mach_device_reference: the record's.
            device: Reference::clone(device),
            offset,
            protection,
            port,
            link: LinkedListAtomicLink::new(),
        })
        .unwrap_or_else(|_| panic!("device_pager_setup: KERN_RESOURCE_SHORTAGE, not yet"));
    let address = CacheBox::into_raw(pager);
    // SAFETY: the box's object, given up, stays allocated while listed.
    let pager = unsafe { UnsafeRef::from_raw(address.as_ptr()) };
    // dev_pager_hash_insert: its port stands for it.
    pager
        .port
        .set_kernel_object(KernelObject::DevicePager(address));
    let send = Port::make_send(&pager.port);
    // dev_device_hash_insert, unless a pager was made meanwhile.
    let mut state = device.state.lock();
    assert!(
        state.pagers.iter().all(|other| other.offset != offset),
        "device_pager_setup: an existing pager, not yet"
    );
    state.pagers.push_back(pager);
    drop(state);
    send
}
