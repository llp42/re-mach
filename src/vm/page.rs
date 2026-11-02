// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Physical pages.

use core::mem::MaybeUninit;
use core::ops::Range;
use core::{ptr, slice};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};

use super::pmap::MachinePmap as _;
use crate::arch::{self, PAGE_SIZE, PhysAddr};
use crate::println;
use crate::sync::{BootCell, OnceLock, Spinlock};

/// A physical page.
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

/// How many colours each page queue has; a page's colour is its frame
/// number's lowest bits.
const COLORS: usize = 1024;

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    PageQueueAdapter = &'static Page: Page { queue_link => LinkedListAtomicLink }
);

/// The pages of one colour on one queue.
type PageQueue = Spinlock<LinkedList<PageQueueAdapter>>;

/// The page queues, a list per colour each.
struct PageQueues {
    /// Free pages.
    free: [PageQueue; COLORS],
    /// Pages in use, not referenced lately.
    #[expect(dead_code, reason = "pageout moves pages through it")]
    inactive: [PageQueue; COLORS],
    /// Pages in use, referenced lately.
    #[expect(dead_code, reason = "pageout moves pages through it")]
    active: [PageQueue; COLORS],
}

/// Most low memory kept for DMA.
const DMA_RESERVE_LIMIT: usize = 256 << 20;

/// Free frames kept for DMA, below `DMA_RESERVE_LIMIT`: a bit per frame
/// from 0, set when it is free.
static DMA_RESERVE: Spinlock<[u64; DMA_RESERVE_LIMIT / PAGE_SIZE / 64]> =
    Spinlock::new([0; DMA_RESERVE_LIMIT / PAGE_SIZE / 64]);

static QUEUES: PageQueues = PageQueues {
    free: [const { PageQueue::new(LinkedList::new(PageQueueAdapter::NEW)) }; COLORS],
    inactive: [const { PageQueue::new(LinkedList::new(PageQueueAdapter::NEW)) }; COLORS],
    active: [const { PageQueue::new(LinkedList::new(PageQueueAdapter::NEW)) }; COLORS],
};

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

/// Builds the page array over the loaded span and frees the boot heap's
/// unused pages, on the boot CPU: those below a quarter of the span, up to
/// `DMA_RESERVE_LIMIT`, are kept for DMA.
///
/// # Panics
///
/// If no memory is loaded, or on a second call.
pub fn setup() {
    let (span, heap) = {
        let loaded = LOADED.borrow();
        (loaded.span.clone(), loaded.heap.clone())
    };
    let span = span.expect("vm_page: no physical memory loaded");
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
    if let Some(heap) = heap {
        let first = span.start.0 / PAGE_SIZE;
        let reserved = DMA_RESERVE_LIMIT.min((span.end - span.start) / 4) / PAGE_SIZE;
        let range = (heap.start - span.start) / PAGE_SIZE..(heap.end - span.start) / PAGE_SIZE;
        for (page, index) in pages[range.clone()].iter().zip(range) {
            let frame = first + index;
            if frame < reserved {
                DMA_RESERVE.lock()[frame / 64] |= 1 << (frame % 64);
            } else {
                QUEUES.free[frame % COLORS].lock().push_back(page);
            }
        }
    }
}
