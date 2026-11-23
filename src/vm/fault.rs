// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Page faults.

use core::mem::ManuallyDrop;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use super::map::entry::{Backing, Entry};
use super::map::lookup::Lookup;
use super::map::map::Map;
use super::object::{
    object::Object,
    state::{Flags, Kind, ObjectState},
};
use super::page::{self, page::Page};
use super::phys_map::{MachinePhysMap as _, Wiring};
use super::{Protection, STATISTICS, VirtualAddress};
use crate::arch::PAGE_SIZE;
use crate::device::pager::{self, Pager};
use crate::error::Error;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::sched;
use crate::sched::event::Event;
use crate::smp;
use crate::sync::TokenGuard;

/// What a fault is for.
#[derive(Clone, Copy)]
pub enum Purpose {
    /// An access to the page.
    Access,
    /// Giving the page its entry's wiring.
    ChangeWiring,
}

/// Resolves a fault on the page at `address` in `map`, which asked for
/// `access`, for `purpose`: looks the address up, finds the page, checks
/// the map's version, maps the page, then wires it or activates it; on a
/// device's object, `device` maps the device's frame instead.
///
/// # Errors
///
/// As `Map::lookup`.
///
/// # Panics
///
/// As `Map::lookup`, `device`, `fault_page`, `MachinePhysMap::enter` and
/// `Page::activate`, and at the object's last reference as its free. If a
/// lookup after a change to the map fails or finds another page, the
/// object's copy changed, a wired page lost the access it asked for, or for
/// `Purpose::ChangeWiring` if the entry is not wired: not yet.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the lookups' and vm_fault_page's guards are moved out and dropped first"
)]
pub fn fault(map: &Map, address: usize, access: Protection, purpose: Purpose) -> Result<(), Error> {
    let requested = access;
    let mut lookup = map.lookup(VirtualAddress::from(address), access)?;
    // A wired page is faulted in for all its entry allows.
    let access = if lookup.wired {
        lookup.protection
    } else {
        access
    };
    assert!(
        lookup.wired || matches!(purpose, Purpose::Access),
        "vm_fault: unwiring through vm_fault_page's must_be_resident, not yet"
    );
    // The lookup's reference keeps the object while the map changes.
    lookup.state.begin_paging();
    if let Some(Kind::Device(pager)) = &lookup.state.kind {
        let pager = pager.clone();
        device(map, address, access, lookup, &pager);
        return Ok(());
    }
    // A pager's page: asked for if missing, waited for, unlocked.
    let found = if lookup.state.flags.contains(Flags::PAGER_CREATED) {
        let object = Reference::clone(&lookup.object);
        let offset = usize::from(lookup.offset);
        let Some(state) = pager_page(&object, lookup.state, offset, access) else {
            drop(object);
            drop(lookup.object);
            return fault(map, address, requested, purpose);
        };
        drop(object);
        fault_page(&lookup.object, state, offset, access, Residency::Optional)
    } else {
        fault_page(
            &lookup.object,
            lookup.state,
            usize::from(lookup.offset),
            access,
            Residency::Optional,
        )
    };
    // The page's lock keeps its access off the mapping, as `PMAP_ENTER`
    // does.
    let (mut protection, mut wired, mut version) = (
        lookup.protection - found.page.page_lock(),
        lookup.wired,
        lookup.version,
    );
    // The page's object is the lookup's: a read through a shadow is not yet.
    let old_copy = found.state.copy.as_ref().map(Reference::as_ptr);
    drop(found.state);
    let verified = loop {
        if let Some(verified) = map.verify(version) {
            break verified;
        }
        // Not asking for writing: a page no longer writable is mapped without.
        let retry = map
            .lookup(VirtualAddress::from(address), access - Protection::WRITE)
            .unwrap_or_else(|_| {
                panic!("vm_fault: vm_map_lookup failed after vm_map_verify, not yet")
            });
        drop(retry.state);
        let state = lookup.object.lock();
        assert!(
            Reference::as_ptr(&retry.object) == Reference::as_ptr(&lookup.object)
                && retry.offset == lookup.offset,
            "vm_fault: another page after vm_map_verify, RetryFault not yet"
        );
        protection &= retry.protection;
        (wired, version) = (retry.wired, retry.version);
        drop(state);
    };
    let state = lookup.object.lock();
    assert!(
        state.copy.as_ref().map(Reference::as_ptr) == old_copy,
        "vm_fault: the copy object changed, not yet"
    );
    assert!(
        !wired || protection == access,
        "vm_fault: a wired page without its access, RetryFault not yet"
    );
    drop(state);
    let wiring = if wired {
        Wiring::Wired
    } else {
        Wiring::Pageable
    };
    map.phys_map()
        .enter(address, found.page.address(), protection, wiring);
    let mut state = lookup.object.lock();
    match purpose {
        Purpose::ChangeWiring if wired => found.page.wire(),
        Purpose::ChangeWiring => panic!("vm_fault: vm_page_unwire, not yet"),
        Purpose::Access => {
            // Reference bits are kept in software: a page on a paging queue
            // stays.
            if !found.page.is_on_paging_queue() {
                found.page.activate();
            }
            found.page.set_referenced();
            // As vm_fault_page: one that can be written will be.
            if protection.contains(Protection::WRITE) {
                found.page.set_dirty();
            }
        }
    }
    drop(verified);
    found.page.clear_busy();
    state.end_paging();
    drop(state);
    // vm_object_deallocate.
    drop(lookup.object);
    Ok(())
}

/// Maps `address` in `map`, which asked for `access`, to the frame of the
/// device memory there, as `lookup` found it, with a reference and a paging
/// operation of the fault's on its object, whose pager is `pager`; with no
/// page, and the entry's protection, as NetBSD's device pager does. Counts
/// the fault and a pagein, as `vm_fault_page`'s pager path does.
///
/// # Panics
///
/// If no thread runs on the CPU. If the entry is wired, `access` writes,
/// the object has a copy or its pager is not ready, the device refuses the
/// frame, or the map changed since the lookup: not yet. As
/// `MachinePhysMap::enter`, and at the object's last reference as its free.
fn device(map: &Map, address: usize, access: Protection, lookup: Lookup, pager: &Pager) {
    let task = smp::current()
        .active_thread()
        .expect("vm_fault_page: no current thread")
        .task();
    STATISTICS.faults.fetch_add(1, Ordering::Relaxed);
    task.faults.fetch_add(1, Ordering::Relaxed);
    assert!(!lookup.wired, "vm_fault: a wired device entry, not yet");
    assert!(
        !access.contains(Protection::WRITE) && lookup.state.copy.is_none(),
        "vm_fault_page: writing or copying a device object, not yet"
    );
    // look_for_page: no page is resident, and none stands in.
    assert!(
        lookup.state.flags.contains(Flags::PAGER_READY),
        "vm_fault_page: a pager not ready, not yet"
    );
    // Unlocked for the pager; the fault's reference keeps the object, and so
    // its pager.
    drop(lookup.state);
    STATISTICS.pageins.fetch_add(1, Ordering::Relaxed);
    task.pageins.fetch_add(1, Ordering::Relaxed);
    // memory_object_data_request, a direct call.
    let frame = pager::map_page(pager, lookup.offset)
        .unwrap_or_else(|| panic!("vm_fault_page: VM_FAULT_MEMORY_ERROR, not yet"));
    let verified = map
        .verify(lookup.version)
        .unwrap_or_else(|| panic!("vm_fault: vm_map_verify failed on a device object, not yet"));
    map.phys_map()
        .enter(address, frame, lookup.protection, Wiring::Pageable);
    drop(verified);
    let mut state = lookup.object.lock();
    state.end_paging();
    drop(state);
    // vm_object_deallocate.
    drop(lookup.object);
}

/// Wires the pages of `entry` in `map`, while the map is unlocked and the
/// entry in transition: each through `wire_fast`, else by a fault changing
/// its wiring.
///
/// # Panics
///
/// As `wire_fast` and `fault`.
pub(super) fn wire(map: &Map, entry: &Entry) {
    // pmap_pageable has no counterpart: the machine's does nothing.
    let range = usize::from(entry.start())..usize::from(entry.end());
    for address in range.step_by(PAGE_SIZE) {
        if wire_fast(map, address, entry).is_err() {
            // Assumed to succeed, as in GNU Mach.
            let _ = fault(map, address, Protection::empty(), Purpose::ChangeWiring);
        }
    }
}

/// Wires the page at `address` in `entry` of `map` if it is resident and
/// nothing else goes on there, mapping it wired with the entry's
/// protection, as `fault` would with no lookup; counts a fault.
///
/// # Errors
///
/// `Failure` for a submap or a page not resident or busy, to be faulted in.
///
/// # Panics
///
/// If no thread runs on the CPU, or the entry has no object. If the page
/// would be written and its object has a copy: not yet. As
/// `MachinePhysMap::enter`.
fn wire_fast(map: &Map, address: usize, entry: &Entry) -> Result<(), Error> {
    let task = smp::current()
        .active_thread()
        .expect("vm_fault_wire_fast: no current thread")
        .task();
    STATISTICS.faults.fetch_add(1, Ordering::Relaxed);
    task.faults.fetch_add(1, Ordering::Relaxed);
    let Backing::Object { object, offset } = &entry.backing else {
        return Err(Error::Failure);
    };
    let held = NonNull::new(object.load(Ordering::Relaxed)).expect("vm_fault_wire_fast: no object");
    // SAFETY: the entry, in transition, stays with its reference on its
    // object, which this handle borrows and never drops.
    let held = ManuallyDrop::new(unsafe { Reference::from_raw(held) });
    let offset = offset.load(Ordering::Relaxed) + (address - usize::from(entry.start()));
    let protection = entry.protection();
    let mut state = held.lock();
    // A reference of its own keeps the object meanwhile.
    let object = Reference::clone(&held);
    state.begin_paging();
    // With no pager, never absent, in error or locked.
    let Some(page) = state.lookup(offset).filter(|page| !page.is_busy()) else {
        // Given up: the slow way finds the page.
        state.end_paging();
        drop(state);
        drop(object);
        return Err(Error::Failure);
    };
    page.wire();
    page.set_busy();
    assert!(
        state.copy.is_none() || !protection.contains(Protection::WRITE),
        "vm_fault_wire_fast: writing with a copy object, not yet"
    );
    // Unlocked, as entering may fault.
    drop(state);
    map.phys_map()
        .enter(address, page.address(), protection, Wiring::Wired);
    let mut state = object.lock();
    page.clear_busy();
    state.end_paging();
    drop(state);
    drop(object);
    Ok(())
}

/// Unwires the pages of `entry` in `map`, while the map is write-locked: each
/// loses its mapping's wiring, then `fault_page` finds it, resident, to
/// unwire it. A kernel stack's first page, its guard, is skipped (no GNU
/// Mach counterpart).
///
/// # Panics
///
/// For a submap, or an entry with no object: not yet. As
/// `MachinePhysMap::change_wiring`, `fault_page` and `Page::unwire`.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the object's guard is moved into fault_page, then out and dropped"
)]
pub(super) fn unwire(map: &Map, entry: &Entry) {
    let Backing::Object { object, offset } = &entry.backing else {
        panic!("vm_fault_unwire: vm_fault on a submap, not yet");
    };
    // SAFETY: the entry holds a reference on its object, and the map's write
    // lock, held through the call, keeps the entry and so that reference.
    let object = unsafe { object.load(Ordering::Relaxed).as_ref() }
        .expect("vm_fault_unwire: vm_fault with no object, not yet");
    let start = usize::from(entry.start());
    let first = if entry.kernel_stack {
        start + PAGE_SIZE
    } else {
        start
    };
    for address in (first..usize::from(entry.end())).step_by(PAGE_SIZE) {
        map.phys_map().change_wiring(address, Wiring::Pageable);
        let mut state = object.lock();
        state.begin_paging();
        let offset = offset.load(Ordering::Relaxed) + (address - start);
        let Found { page, mut state } = fault_page(
            object,
            state,
            offset,
            Protection::empty(),
            Residency::Required,
        );
        page.unwire();
        page.clear_busy();
        state.end_paging();
        drop(state);
    }
    // pmap_pageable has no counterpart: the machine's does nothing.
}

/// What `fault_page` finds.
struct Found {
    /// The page, busy.
    page: &'static Page,
    /// The state of the page's object, still locked, with the call's paging
    /// operation.
    state: TokenGuard<'static, ObjectState>,
}

/// Whether the page `fault_page` finds must be resident.
#[derive(Clone, Copy)]
enum Residency {
    /// It must: it is wired.
    Required,
    /// It need not: one missing is filled with zeros.
    Optional,
}

/// Waits for `object`, whose page at `offset` is to be faulted for
/// `access`, to have it: its pager is asked for a missing page, an unlock
/// the page's lock holds off is asked for, and the fault sleeps on the
/// object until its page data or an unlock comes, as `vm_fault_page`'s
/// `look_for_page` does. Gives `state`, still locked, with the call's
/// paging operation, once the page is resident and open to `access`, or
/// absent and unavailable, which `fault_page` fills with zeros; `None`
/// once it waited, so the fault looks again.
///
/// # Panics
///
/// If its data is in error, the object has no pager or request port, or no
/// page can be allocated: not yet.
fn pager_page(
    object: &Reference<Object>,
    mut state: TokenGuard<'static, ObjectState>,
    offset: usize,
    access: Protection,
) -> Option<TokenGuard<'static, ObjectState>> {
    let event = Event::of(&**object);
    loop {
        // Its pager said it is ready before its data is asked for.
        if state.flags.contains(Flags::PAGER_READY) {
            match state.lookup(offset) {
                // Its data is in error: no page comes.
                Some(page) if page.has_error() => {
                    state.end_paging();
                    drop(state);
                    panic!("vm_fault_page: VM_FAULT_MEMORY_ERROR, not yet");
                }
                // Resident, unless its lock holds the access off.
                Some(page) if !page.is_absent() && !page.is_busy() => {
                    let lock = page.page_lock();
                    if !access.intersects(lock) {
                        return Some(state);
                    }
                    let asked = page.unlock_request();
                    if !asked.contains(access) {
                        // Asked for at once, then looped to wait for it.
                        let unlock = asked | access;
                        page.set_unlock_request(unlock);
                        let (pager, request) = pager_ports(&state);
                        state.end_paging();
                        drop(state);
                        super::pager::data_unlock(
                            pager,
                            request,
                            offset,
                            PAGE_SIZE,
                            i32::try_from(unlock.bits()).unwrap(),
                        );
                        state = lock_object(object);
                        continue;
                    }
                }
                // Absent and not busy: its pager said it is unavailable, and
                // `fault_page` fills it with zeros.
                Some(page) if page.is_absent() && !page.is_busy() => return Some(state),
                // Absent and busy, or busy: its data is on the way.
                Some(_) => {}
                // No page: a busy, absent placeholder waits for the data.
                None => {
                    let page = page::alloc::allocate(offset / PAGE_SIZE)
                        .expect("vm_fault_page: VM_FAULT_MEMORY_SHORTAGE, not yet");
                    page.set_absent();
                    page.set_busy();
                    state.insert(object, page, offset);
                    let (pager, request) = pager_ports(&state);
                    state.end_paging();
                    drop(state);
                    super::pager::data_request(
                        pager,
                        request,
                        offset,
                        PAGE_SIZE,
                        i32::try_from(access.bits()).unwrap(),
                    );
                    state = lock_object(object);
                    continue;
                }
            }
        }
        // Waits with the object locked, so that no data comes in before.
        sched::wait::assert_wait(Some(event));
        state.end_paging();
        drop(state);
        sched::switch::block();
        state = lock_object(object);
    }
}

/// The state of `object`, locked, with a paging operation begun.
///
/// # Safety
///
/// The caller's reference keeps `object`, and its state, alive.
fn lock_object(object: &Reference<Object>) -> TokenGuard<'static, ObjectState> {
    // SAFETY: as `Map::lookup`: the reference keeps the object, and so its
    // state, while the guard lives.
    let mut state = unsafe { Reference::as_ptr(object).as_ref() }.lock();
    state.begin_paging();
    state
}

/// The object's pager's send right, a copy of it, and its request port's,
/// as a call to its pager takes them.
///
/// # Panics
///
/// If it has no pager or request port.
fn pager_ports(state: &ObjectState) -> (Reference<Port>, Reference<Port>) {
    let pager = state.pager.as_ref().expect("vm_fault_page: no pager");
    let request = state
        .pager_request
        .as_ref()
        .expect("vm_fault_page: no pager request port");
    (Port::copy_send(pager), Port::make_send(request))
}

/// Finds the page at `offset` in `object`, whose `state` is locked and
/// holds a paging operation for the call, for `access`; so far one
/// resident there, or, as `residency` allows, one `object` lacks, which it
/// fills there with zeros or, for a write, with a copy of its shadow's
/// page.
///
/// # Panics
///
/// If no thread runs on the CPU. If a page is busy, it must be resident and
/// is not, an object has a pager or a copy, the shadow lacks the page and
/// has a shadow, its page is read, or if no page is free: not yet. As
/// `Page::deactivate` and `ObjectState::collapse`.
fn fault_page(
    object: &Object,
    mut state: TokenGuard<'static, ObjectState>,
    offset: usize,
    access: Protection,
    residency: Residency,
) -> Found {
    let task = smp::current()
        .active_thread()
        .expect("vm_fault_page: no current thread")
        .task();
    STATISTICS.faults.fetch_add(1, Ordering::Relaxed);
    task.faults.fetch_add(1, Ordering::Relaxed);
    if let Some(page) = state.lookup(offset) {
        assert!(!page.is_busy(), "vm_fault_page: a busy page, not yet");
        // With no pager, never absent, in error or locked; reference bits
        // are kept in software, so it stays on its paging queue.
        page.set_busy();
        // Its pager said it is unavailable: filled with zeros, as GNU Mach
        // does at the bottom of a shadow chain.
        if page.is_absent() {
            // SAFETY: the page is busy and nothing maps it, so it is ours
            // alone.
            unsafe { page.zero_fill() };
            STATISTICS.zero_fill_count.fetch_add(1, Ordering::Relaxed);
            task.zero_fills.fetch_add(1, Ordering::Relaxed);
            page.clear_absent();
        }
        assert!(
            state.copy.is_none(),
            "vm_fault_page: a copy object, not yet"
        );
        return Found { page, state };
    }
    assert!(
        matches!(residency, Residency::Optional),
        "vm_fault_page: must_be_resident, no page in the first object, not yet"
    );
    // The placeholder: a real page, busy, where GNU Mach's is fictitious.
    // Objects have no colour of their own yet.
    let page = page::alloc::allocate(offset / PAGE_SIZE)
        .expect("vm_fault_page: VM_FAULT_MEMORY_SHORTAGE, not yet");
    page.set_busy();
    state.insert(object, page, offset);
    assert!(
        !state.flags.contains(Flags::PAGER_CREATED),
        "vm_fault_page: a pager (look_for_page), not yet"
    );
    // Down the shadow chain; at its end, the placeholder is filled with zeros.
    let state = 'chain_end: {
        let Some(shadow) = &state.shadow else {
            break 'chain_end state;
        };
        // SAFETY: the object, which the caller keeps, holds a reference on
        // its shadow, and the call's paging operation keeps `collapse` from
        // changing it until the walk is done.
        let shadow: &Object = unsafe { Reference::as_ptr(shadow).as_ref() };
        let shadow_offset = offset + usize::from(state.shadow_offset);
        let token = TokenGuard::mutex(&state);
        // One token at a time: the busy placeholder keeps the place.
        drop(state);
        let mut shadow_state = shadow.lock();
        shadow_state.begin_paging();
        let Some(source) = shadow_state.lookup(shadow_offset) else {
            assert!(
                !shadow_state.flags.contains(Flags::PAGER_CREATED),
                "vm_fault_page: a pager (look_for_page), not yet"
            );
            assert!(
                shadow_state.shadow.is_none(),
                "vm_fault_page: the next object in a shadow chain, not yet"
            );
            // Back to the first object, whose placeholder is filled.
            shadow_state.end_paging();
            drop(shadow_state);
            break 'chain_end token.lock();
        };
        assert!(!source.is_busy(), "vm_fault_page: a busy page, not yet");
        source.set_busy();
        assert!(
            access.contains(Protection::WRITE),
            "vm_fault_page: a read through a shadow object, not yet"
        );
        // Unlocked for the copy: both pages are busy.
        drop(shadow_state);
        // SAFETY: the placeholder is busy and nothing maps it, so it is ours
        // alone; the busy source stays, and copy-on-write maps it nowhere
        // writable.
        unsafe { page.copy_from(source) };
        let mut shadow_state = shadow.lock();
        // Left mapped, as DragonFly leaves it: entering the copy replaces
        // the faulting address's mapping.
        source.deactivate();
        source.clear_busy();
        shadow_state.end_paging();
        drop(shadow_state);
        STATISTICS.cow_faults.fetch_add(1, Ordering::Relaxed);
        task.cow_faults.fetch_add(1, Ordering::Relaxed);
        // The copy is the placeholder, in its place already.
        let mut state = token.lock();
        state.end_paging();
        ObjectState::collapse(&mut state, object);
        state.begin_paging();
        assert!(
            state.copy.is_none(),
            "vm_fault_page: a copy object, not yet"
        );
        return Found { page, state };
    };
    // SAFETY: the placeholder is busy and nothing maps it, so it is ours alone.
    unsafe { page.zero_fill() };
    STATISTICS.zero_fill_count.fetch_add(1, Ordering::Relaxed);
    task.zero_fills.fetch_add(1, Ordering::Relaxed);
    assert!(
        state.copy.is_none(),
        "vm_fault_page: a copy object, not yet"
    );
    Found { page, state }
}
