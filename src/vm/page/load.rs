// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::MaybeUninit;
use core::ops::Range;
use core::{ptr, slice};

use super::page::Page;
use super::pool::{PAGES, PageArray, Pool, pages};
use super::queue::{QUEUES, color};
use super::reserve::{DMA_RESERVE, reserve_end};
use crate::arch::{self, PAGE_SIZE, PhysAddr};
use crate::println;
use crate::sync::BootCell;
use crate::vm::phys_map::MachinePhysMap as _;

/// Physical memory loaded from boot memory.
struct Loaded {
    /// The memory the page array will cover, holes included.
    span: Option<Range<PhysAddr>>,
    /// The boot heap's unused part, free from the start.
    heap: Option<Range<PhysAddr>>,
    /// The bytes of the segments loaded, holes within them included.
    size: usize,
}

static LOADED: BootCell<Loaded> = BootCell::new(Loaded {
    span: None,
    heap: None,
    size: 0,
});

/// Loads the segment `range`, widening the span to cover it.
pub fn load(range: Range<PhysAddr>) {
    let mut loaded = LOADED.borrow_mut();
    loaded.size += range.end - range.start;
    loaded.span = Some(widen(loaded.span.take(), range));
}

/// The bytes of the segments loaded, holes within them included.
pub fn memory_size() -> usize {
    LOADED.borrow().size
}

/// Widens the free heap to cover `range`.
pub fn load_heap(range: Range<PhysAddr>) {
    let mut loaded = LOADED.borrow_mut();
    loaded.heap = Some(widen(loaded.heap.take(), range));
}

/// `range`, widened to cover `area` if there is one.
fn widen(area: Option<Range<PhysAddr>>, range: Range<PhysAddr>) -> Range<PhysAddr> {
    match area {
        Some(area) => area.start.min(range.start)..area.end.max(range.end),
        None => range,
    }
}

/// Builds the page array over the loaded span and frees the boot heap's
/// unused pages, on the boot CPU.
///
/// # Panics
///
/// If no memory is loaded, or on a second call.
pub fn setup() {
    let span = LOADED
        .borrow()
        .span
        .clone()
        .expect("vm_page: no physical memory loaded");
    let count = (span.end - span.start) / PAGE_SIZE;
    let size = (count * size_of::<Page>()).next_multiple_of(PAGE_SIZE);
    println!(
        "vm_page: page table size: {count} entries ({}k)",
        size >> 10
    );
    #[expect(clippy::cast_ptr_alignment, reason = "stolen memory is page-aligned")]
    let slots = arch::PhysMap::steal_memory(size).cast::<MaybeUninit<Page>>();
    // SAFETY: the stolen memory is the kernel's for good, page-aligned,
    // mapped, and large enough for `count` pages.
    let slots = unsafe { slice::from_raw_parts_mut(slots, count) };
    for slot in &mut *slots {
        slot.write(Page::new());
    }
    // SAFETY: every slot is written, and a `MaybeUninit<Page>` is laid out
    // as a `Page`.
    let pages = unsafe { &*ptr::slice_from_raw_parts(slots.as_ptr().cast::<Page>(), count) };
    let reserve_pages = reserve_end(count * PAGE_SIZE)
        .saturating_sub(span.start.0 / PAGE_SIZE)
        .min(count);
    assert!(
        PAGES
            .set(PageArray {
                start: span.start,
                pages,
                reserve: Pool::new(reserve_pages),
                queues: Pool::new(count - reserve_pages),
            })
            .is_ok(),
        "vm_page: set up twice"
    );
    // Stealing the array closed the boot heap.
    let heap = LOADED.borrow().heap.clone();
    if let Some(heap) = heap {
        release(heap);
    }
}

/// Frees the whole pages of `range`, which boot memory kept: those below a
/// quarter of the span, up to `DMA_RESERVE_LIMIT`, are kept for DMA.
///
/// # Panics
///
/// If the page array is not set up, or does not cover `range`.
pub fn release(range: Range<PhysAddr>) {
    let array = pages();
    let first = array.start.0 / PAGE_SIZE;
    let reserved = reserve_end(array.pages.len() * PAGE_SIZE);
    for frame in range.start.0.div_ceil(PAGE_SIZE)..range.end.0 / PAGE_SIZE {
        if frame < reserved {
            let mut reserve = DMA_RESERVE.lock();
            reserve.put(frame);
            drop(reserve);
        } else {
            QUEUES.free.push(color(frame), &array.pages[frame - first]);
        }
    }
}
