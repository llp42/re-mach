// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr;
use core::sync::atomic::Ordering;

use super::page::{CACHE, Flags, Page};
use super::pool::pages;
use super::queue::{COLORS, QUEUES, color};
use super::reserve::{DMA_RESERVE, allocate_reserved, reserve_end};
use crate::arch::{PAGE_SIZE, PhysAddr};
use crate::{machine, smp};

/// The bytes the page array covers, holes included.
///
/// # Panics
///
/// If the page array is not set up.
pub fn span_size() -> usize {
    pages().pages.len() * PAGE_SIZE
}

/// The page holding `address`; `None` if the page array does not cover it.
///
/// # Panics
///
/// If the page array is not set up.
pub fn lookup(address: PhysAddr) -> Option<&'static Page> {
    let array = pages();
    array
        .pages
        .get(address.checked_sub(array.start)? / PAGE_SIZE)
}

/// Sets up the pages' second pass, once the VM is up, on the boot CPU.
pub fn init() {
    CACHE.register();
}

/// Takes a free page from the colour `index` picks among this CPU's, else
/// from the aligned groups of 16, 32, … colours around it, or around its
/// hint, up to all, else from the DMA reserve; `None` when no page is free.
/// `index` is the page's index in its object plus the object's colour.
pub fn allocate(index: usize) -> Option<&'static Page> {
    let cpu_colors = COLORS / machine::processor_count();
    let first = smp::current().id() * cpu_colors + index % cpu_colors;
    let queue = &QUEUES.free.queues[first];
    if let Some(page) = QUEUES.free.pop(first) {
        queue.last.store(0, Ordering::Relaxed);
        return Some(page);
    }
    let start = match queue.last.load(Ordering::Relaxed) {
        0 => first,
        last => last - 1,
    };
    // Each group from the start, wrapping, skipping the group before it.
    let mut searched = start..start;
    let mut looked = 0;
    let mut size = 16;
    while size <= COLORS {
        let group = start - start % size;
        for color in (0..size).map(|offset| group + (start + offset) % size) {
            if searched.contains(&color) {
                continue;
            }
            if QUEUES.free.pages.load(Ordering::Relaxed) == 0 {
                return allocate_reserved();
            }
            if let Some(page) = QUEUES.free.pop(color) {
                if looked >= 8 {
                    queue.last.store(color + 1, Ordering::Relaxed);
                }
                return Some(page);
            }
            looked += 1;
        }
        searched = group..group + size;
        size *= 2;
    }
    allocate_reserved()
}

/// Takes `count` contiguous free pages whose frames lie in `first` up to
/// `last`, from the DMA reserve, as `vm_page_grab_contig` does for its
/// segment; none when there is no such run.
///
/// # Panics
///
/// If the page array is not set up.
pub fn allocate_contiguous(count: usize, first: usize, last: usize) -> Option<&'static [Page]> {
    let array = pages();
    let base = array.start.0 / PAGE_SIZE;
    let first = first.max(base);
    let last = last
        .min(base + array.pages.len())
        .min(reserve_end(array.pages.len() * PAGE_SIZE));
    let start = DMA_RESERVE.lock().take_contiguous(count, first, last)?;
    Some(&array.pages[start - base..][..count])
}

/// Puts `page`, which `allocate` gave, back in its pool once it is in no
/// object, on no paging queue and unused, as `vm_page_free` does: unwired,
/// not busy and reset as new, it goes to the DMA reserve for a frame below
/// its end, else to its colour's free queue.
pub fn free(page: &'static Page) {
    if page.wire_count.load(Ordering::Relaxed) != 0 {
        QUEUES.wired_pages.fetch_sub(1, Ordering::Relaxed);
        page.wire_count.store(0, Ordering::Relaxed);
    }
    page.clear_busy();
    // vm_page_init.
    page.object.store(ptr::null_mut(), Ordering::Relaxed);
    page.offset.store(0, Ordering::Relaxed);
    page.flags.store(Flags::empty().bits(), Ordering::Relaxed);
    let frame = page.address().0 / PAGE_SIZE;
    if frame < reserve_end(pages().pages.len() * PAGE_SIZE) {
        let mut reserve = DMA_RESERVE.lock();
        reserve.put(frame);
        drop(reserve);
    } else {
        QUEUES.free.push(color(frame), page);
    }
}

/// Takes a free page as `allocate` does for `index`, and fills it with
/// zeros; `None` when no page is free.
pub fn allocate_zeroed(index: usize) -> Option<&'static Page> {
    let page = allocate(index)?;
    // SAFETY: the page is free, so the kernel's alone.
    unsafe { page.zero_fill() };
    Some(page)
}
