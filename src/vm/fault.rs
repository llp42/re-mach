// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Page faults.

use core::ptr;
use core::sync::atomic::Ordering;

use super::map::{Backing, Entry, Map};
use super::object::{Flags, Object, ObjectState};
use super::page::{self, Page};
use super::phys_map::{MachinePhysMap as _, Wiring};
use super::{Protection, STATISTICS, VirtualAddress};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
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
/// the map's version, maps the page, then wires it or activates it.
///
/// # Errors
///
/// As `Map::lookup`.
///
/// # Panics
///
/// As `Map::lookup`, `fault_page`, `MachinePhysMap::enter`,
/// `Page::activate` and `Object::deallocate`. If a lookup after a change to
/// the map fails or finds another page, the object's copy changed, a wired
/// page lost the access it asked for, or for `Purpose::ChangeWiring` if
/// the entry is not wired: not yet.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the lookups' and vm_fault_page's guards are moved out and dropped first"
)]
pub fn fault(map: &Map, address: usize, access: Protection, purpose: Purpose) -> Result<(), Error> {
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
    // A reference of its own keeps the object while the map changes.
    lookup.object.reference();
    lookup.state.begin_paging();
    let found = fault_page(
        lookup.object,
        lookup.state,
        usize::from(lookup.offset),
        Residency::Optional,
    );
    let (mut protection, mut wired, mut version) =
        (lookup.protection, lookup.wired, lookup.version);
    // With no shadows, the page's object is the lookup's.
    let old_copy = found.state.copy.map(ptr::from_ref);
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
            ptr::eq(retry.object, lookup.object) && retry.offset == lookup.offset,
            "vm_fault: another page after vm_map_verify, RetryFault not yet"
        );
        protection &= retry.protection;
        (wired, version) = (retry.wired, retry.version);
        drop(state);
    };
    let state = lookup.object.lock();
    assert!(
        state.copy.map(ptr::from_ref) == old_copy,
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
        }
    }
    drop(verified);
    found.page.clear_busy();
    state.end_paging();
    drop(state);
    lookup.object.deallocate();
    Ok(())
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
    // SAFETY: the entry holds a reference on its object, which lives until
    // vm_object_deallocate frees it.
    let object =
        unsafe { object.load(Ordering::Relaxed).as_ref() }.expect("vm_fault_wire_fast: no object");
    let offset = offset.load(Ordering::Relaxed) + (address - usize::from(entry.start()));
    let protection = entry.protection();
    let mut state = object.lock();
    // A reference of its own keeps the object meanwhile.
    object.reference();
    state.begin_paging();
    // With no pager, never absent, in error or locked.
    let Some(page) = state.lookup(offset).filter(|page| !page.is_busy()) else {
        // Given up: the slow way finds the page.
        state.end_paging();
        drop(state);
        object.deallocate();
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
    object.deallocate();
    Ok(())
}

/// Unwires the pages of `entry` in `map`, while the map is write-locked: each
/// loses its mapping's wiring, then `fault_page` finds it, resident, to
/// unwire it.
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
    for address in (start..usize::from(entry.end())).step_by(PAGE_SIZE) {
        map.phys_map().change_wiring(address, Wiring::Pageable);
        let mut state = object.lock();
        state.begin_paging();
        let offset = offset.load(Ordering::Relaxed) + (address - start);
        let Found { page, mut state } = fault_page(object, state, offset, Residency::Required);
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

/// Finds the page at `offset` in `object`, whose `state` is locked and
/// holds a paging operation for the call; so far one resident there, or,
/// as `residency` allows, one `object` lacks, which it fills with zeros
/// there.
///
/// # Panics
///
/// If no thread runs on the CPU. If the page is busy, it must be resident
/// and is not, `object` has a pager, a shadow or a copy, or if no page is
/// free: not yet.
fn fault_page(
    object: &'static Object,
    mut state: TokenGuard<'static, ObjectState>,
    offset: usize,
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
    let page = page::allocate(offset / PAGE_SIZE)
        .expect("vm_fault_page: VM_FAULT_MEMORY_SHORTAGE, not yet");
    page.set_busy();
    state.insert(object, page, offset);
    assert!(
        !state.flags.contains(Flags::PAGER_CREATED),
        "vm_fault_page: a pager (look_for_page), not yet"
    );
    assert!(
        state.shadow.is_none(),
        "vm_fault_page: a shadow object, not yet"
    );
    // SAFETY: the page was free and nothing maps it, so it is ours alone.
    unsafe { page.zero_fill() };
    STATISTICS.zero_fill_count.fetch_add(1, Ordering::Relaxed);
    task.zero_fills.fetch_add(1, Ordering::Relaxed);
    assert!(
        state.copy.is_none(),
        "vm_fault_page: a copy object, not yet"
    );
    Found { page, state }
}
