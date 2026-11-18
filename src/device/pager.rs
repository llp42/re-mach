// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The device pager, which backs memory objects mapped from devices; so
//! far only its records, their making, setting up and termination, and the
//! frames they map.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use intrusive_collections::{LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::{Device, Kind, time};
use crate::arch::PhysAddr;
use crate::ipc::port::{KernelObject, Port};
use crate::reference::Reference;
use crate::slab::{Cache, CacheBox};
use crate::vm::Protection;
use crate::vm::object::{self, CopyStrategy, Offset};

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
/// offset and protection, its port, its object's rights, and its place in
/// its device's pager list, which owns it.
pub struct Pager {
    /// Its device, which it holds a reference to.
    device: Reference<Device>,
    /// Where it starts in its device's memory.
    offset: Offset,
    /// What it allows.
    protection: Protection,
    /// Its port, the memory object.
    port: Reference<Port>,
    /// How many objects it serves.
    client_count: AtomicU32,
    /// A send right for its object's request port, as a reference given up
    /// by `into_raw`; null until it serves one.
    request: AtomicPtr<Port>,
    /// A send right for its object's name port, as a reference given up by
    /// `into_raw`; null until it serves one.
    name: AtomicPtr<Port>,
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
            client_count: AtomicU32::new(0),
            request: AtomicPtr::new(ptr::null_mut()),
            name: AtomicPtr::new(ptr::null_mut()),
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

/// Sets `pager` up for its object, as the kernel's `memory_object_init`
/// reaches `device_pager_init_pager`: one client, whose send rights
/// `request` and `name`, for its request and name ports, it keeps; then
/// readies the object, not to persist and copied with no help.
///
/// # Panics
///
/// If `pager` serves an object already. As `object::ready`.
pub fn init_pager(pager: &Pager, request: Reference<Port>, name: Reference<Port>) {
    assert!(
        pager.client_count.load(Ordering::Relaxed) == 0
            && pager.request.load(Ordering::Relaxed).is_null()
            && pager.name.load(Ordering::Relaxed).is_null(),
        "device_pager_init_pager: a client already"
    );
    pager.client_count.store(1, Ordering::Relaxed);
    let request = Reference::into_raw(request);
    pager.request.store(request.as_ptr(), Ordering::Release);
    pager
        .name
        .store(Reference::into_raw(name).as_ptr(), Ordering::Release);
    // A character device's pager: not cached, no copy help.
    // SAFETY: the record holds the reference `request` gave up.
    object::ready(unsafe { request.as_ref() }, CopyStrategy::None);
}

/// The frame at `offset` in `pager`'s memory, as `device_map_page` gives
/// it; `None` if the device refuses it.
pub fn map_page(pager: &Pager, offset: Offset) -> Option<PhysAddr> {
    let offset = pager.offset + usize::from(offset);
    match pager.device.kind {
        Kind::Time => time::map(offset, pager.protection),
    }
}

/// Terminates `record`, the pager of an object being terminated, whose
/// port is `pager`, as the kernel's `memory_object_terminate` reaches
/// `device_pager_terminate`: off its device's list, the send rights it
/// kept released, its object's request and name ports, whose receive
/// rights `request` and `name` it takes, destroyed, its own port
/// destroyed, then freed with its device's reference.
///
/// # Safety
///
/// Nothing uses `record` after.
///
/// # Panics
///
/// If `pager` is not its port, or it serves another client. As
/// `Port::deallocate_kernel`.
pub unsafe fn terminate(
    record: UnsafeRef<Pager>,
    pager: &Port,
    request: Reference<Port>,
    name: Reference<Port>,
) {
    // dev_pager_hash_lookup's record, which its port stood for.
    assert!(
        Reference::as_ptr(&record.port) == NonNull::from(pager),
        "device_pager_terminate: another pager"
    );
    assert!(
        record.client_count.load(Ordering::Relaxed) == 1
            && record.request.load(Ordering::Relaxed) == Reference::as_ptr(&request).as_ptr()
            && record.name.load(Ordering::Relaxed) == Reference::as_ptr(&name).as_ptr(),
        "device_pager_terminate: another client"
    );
    // dev_pager_hash_delete and dev_device_hash_delete: the list lets it go.
    let address = UnsafeRef::into_raw(record);
    // SAFETY: the record stays allocated while listed.
    let device = unsafe { &(*address).device };
    let mut state = device.state.lock();
    // SAFETY: the record is on its device's list.
    let record = unsafe { state.pagers.cursor_mut_from_ptr(address) }
        .remove()
        .expect("device_pager_terminate: not listed");
    drop(state);
    // The send rights device_pager_init_pager kept.
    // SAFETY: each slot holds the reference its right gave up, taken here.
    let saved = |slot: &AtomicPtr<Port>| unsafe {
        Reference::from_raw(NonNull::new(slot.swap(ptr::null_mut(), Ordering::Acquire)).unwrap())
    };
    Port::release_send(saved(&record.request));
    Port::release_send(saved(&record.name));
    // ipc_port_release_receive on the request and name ports.
    request.deallocate_kernel();
    drop(request);
    name.deallocate_kernel();
    drop(name);
    // ipc_port_dealloc_kernel: the kernel's right for its own port; the
    // record's reference goes with it below.
    record.port.deallocate_kernel();
    // dev_pager_deallocate; its device's reference goes with it, as
    // mach_device_deallocate.
    // SAFETY: records are `CACHE` boxes `setup` gave up with `into_raw`, and
    // off its list nothing else uses this one.
    drop(unsafe { CacheBox::from_raw(&CACHE, NonNull::new(UnsafeRef::into_raw(record)).unwrap()) });
}
