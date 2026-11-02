// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Physical pages.

use core::mem::MaybeUninit;
use core::ops::Range;
use core::{ptr, slice};

use intrusive_collections::LinkedListAtomicLink;

use super::pmap::MachinePmap as _;
use crate::arch::{self, PAGE_SIZE, PhysAddr};
use crate::println;
use crate::sync::{BootCell, OnceLock};

/// A physical page.
#[expect(dead_code, reason = "the page queues link it")]
pub struct Page {
    /// Its place in the page queue it is on.
    queue_link: LinkedListAtomicLink,
}

impl Page {
    /// One on no queue.
    const fn new() -> Self {
        Self {
            queue_link: LinkedListAtomicLink::new(),
        }
    }
}

/// Every page of the loaded span, holes included.
#[expect(dead_code, reason = "the page allocator reads them")]
struct PageArray {
    /// Where the first page is: the span's start.
    start: PhysAddr,
    /// The pages, in address order.
    pages: &'static [Page],
}

/// The page array, once `setup` builds it.
static PAGES: OnceLock<PageArray> = OnceLock::new();

/// Physical memory loaded from boot memory.
struct Loaded {
    /// The memory the page array will cover, holes included.
    span: Option<Range<PhysAddr>>,
    /// The boot heap's unused part, free from the start.
    heap: Option<Range<PhysAddr>>,
}

static LOADED: BootCell<Loaded> = BootCell::new(Loaded {
    span: None,
    heap: None,
});

/// Widens the span to cover `range`.
pub fn load(range: Range<PhysAddr>) {
    let mut loaded = LOADED.borrow_mut();
    loaded.span = Some(widen(loaded.span.take(), range));
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

/// Builds the page array over the loaded span, on the boot CPU.
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
    let slots = arch::Pmap::steal_memory(size).cast::<MaybeUninit<Page>>();
    // SAFETY: the stolen memory is the kernel's for good, page-aligned,
    // mapped, and large enough for `count` pages.
    let slots = unsafe { slice::from_raw_parts_mut(slots, count) };
    for slot in &mut *slots {
        slot.write(Page::new());
    }
    // SAFETY: every slot is written, and a `MaybeUninit<Page>` is laid out
    // as a `Page`.
    let pages = unsafe { &*ptr::slice_from_raw_parts(slots.as_ptr().cast::<Page>(), count) };
    assert!(
        PAGES
            .set(PageArray {
                start: span.start,
                pages,
            })
            .is_ok(),
        "vm_page: set up twice"
    );
}
