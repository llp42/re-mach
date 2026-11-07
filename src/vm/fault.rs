// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Page faults.

use core::sync::atomic::Ordering;

use super::map::Map;
use super::object::{Object, ObjectState};
use super::page::{self, Page};
use super::{Protection, STATISTICS};
use crate::arch::{self, Machine as _, PAGE_SIZE};
use crate::error::Error;
use crate::smp;
use crate::sync::TokenGuard;

/// Resolves a fault on the page at `address` in `map`, which asked for
/// `access`; so far it looks the address up and finds the page, then stops
/// its CPU where it checks the map's version.
///
/// # Errors
///
/// As `Map::lookup`.
///
/// # Panics
///
/// As `Map::lookup` and `fault_page`.
#[expect(
    clippy::significant_drop_tightening,
    reason = "vm_fault_page takes the object locked"
)]
pub fn fault(map: &Map, address: usize, access: Protection) -> Result<(), Error> {
    let mut lookup = map.lookup(address, access)?;
    // A reference of its own keeps the object while the map changes.
    lookup.object.reference();
    lookup.state.begin_paging();
    let _found = fault_page(lookup.object, lookup.state, lookup.offset);
    arch::Current::halt()
}

/// What `fault_page` finds.
#[expect(dead_code, reason = "vm_fault reads it past vm_fault_page")]
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
