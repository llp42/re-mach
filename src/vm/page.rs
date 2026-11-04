// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Physical pages.

use core::mem::MaybeUninit;
use core::ops::Range;
use core::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};
use core::{ptr, slice};

use intrusive_collections::{
    KeyAdapter, LinkedList, LinkedListAtomicLink, RBTreeAtomicLink, intrusive_adapter,
};

use super::object::Object;
use super::pmap::MachinePmap as _;
use crate::arch::{self, PAGE_SIZE, PhysAddr};
use crate::config::MAX_CPUS;
use crate::kernel::slab::Cache;
use crate::sync::{BootCell, CachePadded, CpuLocal, OnceLock, Spinlock};
use crate::{println, smp};

/// A physical page.
pub struct Page {
    /// Its place in the page queue it is on.
    queue_link: LinkedListAtomicLink,
    /// Its place in its object's tree, while it belongs to one.
    object_link: RBTreeAtomicLink,
    /// The object it belongs to; null while it belongs to none. Changed
    /// only behind the object's token.
    object: AtomicPtr<Object>,
    /// Where it is in its object, in bytes; changed as `object` is.
    offset: AtomicUsize,
}

impl Page {
    /// One on no queue, in no object.
    const fn new() -> Self {
        Self {
            queue_link: LinkedListAtomicLink::new(),
            object_link: RBTreeAtomicLink::new(),
            object: AtomicPtr::new(ptr::null_mut()),
            offset: AtomicUsize::new(0),
        }
    }

    /// Records that it belongs to `object` at `offset`, behind the object's
    /// token.
    pub(super) fn set_object(&self, object: &Object, offset: usize) {
        self.offset.store(offset, Ordering::Relaxed);
        self.object
            .store(ptr::from_ref(object).cast_mut(), Ordering::Relaxed);
    }

    /// Where the page is.
    ///
    /// # Panics
    ///
    /// If the page array is not set up.
    pub fn address(&'static self) -> PhysAddr {
        let array = PAGES.get().expect("vm_page: not set up");
        let index = (ptr::from_ref(self).addr() - array.pages.as_ptr().addr()) / size_of::<Self>();
        array.start + index * PAGE_SIZE
    }
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub PageTreeAdapter = &'static Page: Page { object_link => RBTreeAtomicLink }
);

impl<'a> KeyAdapter<'a> for PageTreeAdapter {
    type Key = usize;

    fn get_key(&self, page: &'a Page) -> usize {
        page.offset.load(Ordering::Relaxed)
    }
}

/// Every page of the loaded span, holes included.
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
struct PageQueue {
    /// How many pages its list holds; changed only under the list's lock,
    /// and read without it to pass an empty queue by.
    pages: AtomicUsize,
    /// The pages.
    list: Spinlock<LinkedList<PageQueueAdapter>>,
}

impl PageQueue {
    /// An empty one.
    const fn new() -> Self {
        Self {
            pages: AtomicUsize::new(0),
            list: Spinlock::new(LinkedList::new(PageQueueAdapter::NEW)),
        }
    }

    /// Puts `page` at its end.
    fn push(&self, page: &'static Page) {
        let mut list = self.list.lock();
        list.push_back(page);
        // The count changes only under the lock.
        self.pages
            .store(self.pages.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
        drop(list);
    }

    /// Takes its first page; an empty queue is passed by without its lock.
    fn pop(&self) -> Option<&'static Page> {
        if self.pages.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let mut list = self.list.lock();
        let page = list.pop_front()?;
        // The count changes only under the lock.
        self.pages
            .store(self.pages.load(Ordering::Relaxed) - 1, Ordering::Relaxed);
        drop(list);
        Some(page)
    }
}

/// The page queues, a list per colour each, each on its own cache lines.
struct PageQueues {
    /// Free pages.
    free: [CachePadded<PageQueue>; COLORS],
    /// Pages in use, not referenced lately.
    #[expect(dead_code, reason = "pageout moves pages through it")]
    inactive: [CachePadded<PageQueue>; COLORS],
    /// Pages in use, referenced lately.
    #[expect(dead_code, reason = "pageout moves pages through it")]
    active: [CachePadded<PageQueue>; COLORS],
}

/// Most low memory kept for DMA.
const DMA_RESERVE_LIMIT: usize = 256 << 20;

/// The frame the DMA reserve ends before, for a span of `size` bytes.
fn reserve_end(size: usize) -> usize {
    DMA_RESERVE_LIMIT.min(size / 4) / PAGE_SIZE
}

/// Free frames kept for DMA, below `DMA_RESERVE_LIMIT`: a bit per frame
/// from 0, set when it is free.
static DMA_RESERVE: Spinlock<[u64; DMA_RESERVE_LIMIT / PAGE_SIZE / 64]> =
    Spinlock::new([0; DMA_RESERVE_LIMIT / PAGE_SIZE / 64]);

static QUEUES: PageQueues = PageQueues {
    free: [const { CachePadded::new(PageQueue::new()) }; COLORS],
    inactive: [const { CachePadded::new(PageQueue::new()) }; COLORS],
    active: [const { CachePadded::new(PageQueue::new()) }; COLORS],
};

/// How many colours each CPU looks at first.
const CPU_COLORS: usize = COLORS / MAX_CPUS;

/// Each CPU's count of pages taken, which picks its next colour.
static NEXT_COLOR: CpuLocal<usize> = CpuLocal::new(0);

/// The cache fictitious pages come from: pages outside the page array, such
/// as placeholders.
static CACHE: Cache<Page> = Cache::new("vm_page");

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
    let array = PAGES.get().expect("vm_page: not set up");
    let first = array.start.0 / PAGE_SIZE;
    let reserved = reserve_end(array.pages.len() * PAGE_SIZE);
    for frame in range.start.0.div_ceil(PAGE_SIZE)..range.end.0 / PAGE_SIZE {
        if frame < reserved {
            DMA_RESERVE.lock()[frame / 64] |= 1 << (frame % 64);
        } else {
            QUEUES.free[frame % COLORS].push(&array.pages[frame - first]);
        }
    }
}

/// Prints each pool's pages and free pages: the DMA reserve, then the rest,
/// on the boot CPU.
///
/// # Panics
///
/// If the page array is not set up.
pub fn print_pools() {
    let array = PAGES.get().expect("vm_page: not set up");
    let count = array.pages.len();
    let dma = reserve_end(count * PAGE_SIZE)
        .saturating_sub(array.start.0 / PAGE_SIZE)
        .min(count);
    let dma_free = {
        let reserve = DMA_RESERVE.lock();
        reserve.iter().map(|word| word.count_ones()).sum::<u32>()
    };
    let free = QUEUES
        .free
        .iter()
        .map(|queue| queue.pages.load(Ordering::Relaxed))
        .sum();
    for (name, pages, free) in [
        ("DMA", dma, usize::try_from(dma_free).unwrap()),
        ("DIRECTMAP", count - dma, free),
    ] {
        println!(
            "vm_page: {name}: pages: {pages} ({}M), free: {free} ({}M)",
            (pages * PAGE_SIZE) >> 20,
            (free * PAGE_SIZE) >> 20
        );
    }
}

/// The bytes the page array covers, holes included.
///
/// # Panics
///
/// If the page array is not set up.
pub fn span_size() -> usize {
    PAGES.get().expect("vm_page: not set up").pages.len() * PAGE_SIZE
}

/// Sets up the pages' second pass, once the VM is up, on the boot CPU.
pub fn init() {
    CACHE.register();
}

/// Takes a free page, from this CPU's own colours first; `None` when no page
/// is free.
pub fn allocate() -> Option<&'static Page> {
    let first = {
        let mut next = NEXT_COLOR.lock();
        let first = smp::current().id() * CPU_COLORS + *next % CPU_COLORS;
        *next += 1;
        first
    };
    (0..COLORS).find_map(|offset| QUEUES.free[(first + offset) % COLORS].pop())
}

/// Takes a free page and fills it with zeros; `None` when no page is free.
pub fn allocate_zeroed() -> Option<&'static Page> {
    let page = allocate()?;
    // SAFETY: the page is free, so the kernel's alone, and the direct map
    // reaches all of it.
    unsafe {
        page.address()
            .direct_map_ptr::<u8>()
            .write_bytes(0, PAGE_SIZE);
    }
    Some(page)
}
