// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Page faults.

use core::ptr;
use core::sync::atomic::Ordering;

use super::map::Map;
use super::object::{Object, ObjectState};
use super::page::{self, Page};
use super::pmap::{MachinePmap as _, Wiring};
use super::{Protection, STATISTICS};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::smp;
use crate::sync::TokenGuard;

/// Resolves a fault on the page at `address` in `map`, which asked for
/// `access`: looks the address up, finds the page, checks the map's
/// version, maps the page and activates it.
///
/// # Errors
///
/// As `Map::lookup`.
///
/// # Panics
///
/// As `Map::lookup`, `fault_page`, `MachinePmap::enter`, `Page::activate`
/// and `Object::deallocate`. If a lookup after a change to the map fails or
/// finds another page, the object's copy changed, or a wired page lost the
/// access it asked for: not yet.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the lookups' and vm_fault_page's guards are moved out and dropped first"
)]
pub fn fault(map: &Map, address: usize, access: Protection) -> Result<(), Error> {
    let mut lookup = map.lookup(address, access)?;
    // A reference of its own keeps the object while the map changes.
    lookup.object.reference();
    lookup.state.begin_paging();
    let found = fault_page(lookup.object, lookup.state, lookup.offset);
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
            .lookup(address, access - Protection::WRITE)
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
    map.pmap()
        .enter(address, found.page.address(), protection, wiring);
    let mut state = lookup.object.lock();
    // Reference bits are kept in software: a page on a paging queue stays.
    if !found.page.is_on_paging_queue() {
        found.page.activate();
    }
    found.page.set_referenced();
    drop(verified);
    found.page.wake_done();
    state.end_paging();
    drop(state);
    lookup.object.deallocate();
    Ok(())
}

/// What `fault_page` finds.
struct Found {
    /// The page, busy.
    page: &'static Page,
    /// The state of the page's object, still locked, with the call's paging
    /// operation.
    state: TokenGuard<'static, ObjectState>,
}

/// Finds the page at `offset` in `object`, whose `state` is locked and
/// holds a paging operation for the call; so far only a page `object` lacks,
/// which it fills with zeros there.
///
/// # Panics
///
/// If no thread runs on the CPU. If `object` has the page, a pager, a
/// shadow or a copy, or if no page is free: not yet.
fn fault_page(
    object: &'static Object,
    mut state: TokenGuard<'static, ObjectState>,
    offset: usize,
) -> Found {
    let task = &smp::current()
        .active_thread()
        .expect("vm_fault_page: no current thread")
        .task;
    STATISTICS.faults.fetch_add(1, Ordering::Relaxed);
    task.faults.fetch_add(1, Ordering::Relaxed);
    if let Some(page) = state.lookup(offset) {
        assert!(!page.is_busy(), "vm_fault_page: a busy page, not yet");
        panic!("vm_fault_page: a resident page, not yet");
    }
    // The placeholder: a real page, busy, where GNU Mach's is fictitious.
    // Objects have no colour of their own yet.
    let page = page::allocate(offset / PAGE_SIZE)
        .expect("vm_fault_page: VM_FAULT_MEMORY_SHORTAGE, not yet");
    page.set_busy();
    state.insert(object, page, offset);
    assert!(
        !state.pager_created,
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
