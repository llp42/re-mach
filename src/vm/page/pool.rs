// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::sync::atomic::Ordering;

use super::page::Page;
use super::queue::QUEUES;
use super::reserve::DMA_RESERVE;
use crate::arch::{PAGE_SIZE, PhysAddr};
use crate::println;
use crate::sync::OnceLock;

/// Every page of the loaded span, holes included.
pub(super) struct PageArray {
    /// Where the first page is: the span's start.
    pub(super) start: PhysAddr,
    /// The pages, in address order.
    pub(super) pages: &'static [Page],
    /// The DMA reserve's pool.
    pub(super) reserve: Pool,
    /// The colour queues' pool: the pages above the reserve.
    pub(super) queues: Pool,
}

/// A pool's pages and its free-page thresholds.
pub(super) struct Pool {
    /// Its pages, holes included.
    pages: usize,
    /// Free pages at or below which only privileged allocations proceed.
    min: usize,
    /// Free pages at or below which pageout starts.
    low: usize,
    /// Free pages at or above which pageout stops.
    high: usize,
}

impl Pool {
    /// One of `pages` pages, its thresholds a share of them, each with a
    /// floor.
    pub(super) fn new(pages: usize) -> Self {
        Self {
            pages,
            min: (pages * 5 / 100).max(500),
            low: (pages * 6 / 100).max(600),
            high: (pages * 10 / 100).max(1000),
        }
    }
}

/// The page array, once `setup` builds it.
pub(super) static PAGES: OnceLock<PageArray> = OnceLock::new();

/// The page array.
///
/// # Panics
///
/// If it is not set up.
#[track_caller]
pub(super) fn pages() -> &'static PageArray {
    PAGES.get().expect("vm_page: not set up")
}

/// Prints each pool's pages, free pages and thresholds: the DMA reserve,
/// then the rest, on the boot CPU.
///
/// # Panics
///
/// If the page array is not set up.
pub fn print_pools() {
    let array = pages();
    let reserve_free = DMA_RESERVE.lock().free;
    let queue_free = QUEUES.free.pages.load(Ordering::Relaxed);
    for (name, pool, free) in [
        ("DMA", &array.reserve, reserve_free),
        ("DIRECTMAP", &array.queues, queue_free),
    ] {
        println!(
            "vm_page: {name}: pages: {} ({}M), free: {free} ({}M)",
            pool.pages,
            (pool.pages * PAGE_SIZE) >> 20,
            (free * PAGE_SIZE) >> 20
        );
        println!(
            "vm_page: {name}: min:{} low:{} high:{}",
            pool.min, pool.low, pool.high
        );
    }
}

/// The pages free, active, inactive and wired, as `vm_statistics` reports
/// them.
///
/// # Panics
///
/// If the page array is not set up.
pub(in crate::vm) fn statistics() -> (usize, usize, usize, usize) {
    let free = DMA_RESERVE.lock().free + QUEUES.free.pages.load(Ordering::Relaxed);
    (
        free,
        QUEUES.active.pages.load(Ordering::Relaxed),
        QUEUES.inactive.pages.load(Ordering::Relaxed),
        QUEUES.wired_pages.load(Ordering::Relaxed),
    )
}

/// Whether every pool is usable for unprivileged allocations: it has no
/// page in use to page out, or at least its high threshold of free pages.
/// The DMA reserve's pages are never paged out.
///
/// # Panics
///
/// If the page array is not set up.
pub(in crate::vm) fn usable() -> bool {
    let array = pages();
    QUEUES
        .active
        .queues
        .iter()
        .chain(&QUEUES.inactive.queues)
        .all(|queue| queue.pages.load(Ordering::Relaxed) == 0)
        || QUEUES.free.pages.load(Ordering::Relaxed) >= array.queues.high
}
