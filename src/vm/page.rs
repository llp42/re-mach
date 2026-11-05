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
use crate::kernel::machine;
use crate::kernel::slab::{Cache, Slab};
use crate::sync::{BootCell, CachePadded, OnceLock, Spinlock};
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
    /// Its state while it is a slab.
    pub slab: Slab,
}

impl Page {
    /// One on no queue, in no object.
    const fn new() -> Self {
        Self {
            queue_link: LinkedListAtomicLink::new(),
            object_link: RBTreeAtomicLink::new(),
            object: AtomicPtr::new(ptr::null_mut()),
            offset: AtomicUsize::new(0),
            slab: Slab::new(),
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

/// How many colours each page queue has; a page's colour is the exclusive
/// or of its frame number's three lowest digits in base `COLORS`.
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
    /// Where a search from it last found a page after passing 8 or more
    /// empty queues, as that colour plus one; 0 for none, or once a page is
    /// found on it first. A hint, so Relaxed: it only moves where a search
    /// starts.
    last: AtomicUsize,
}

impl PageQueue {
    /// An empty one.
    const fn new() -> Self {
        Self {
            pages: AtomicUsize::new(0),
            list: Spinlock::new(LinkedList::new(PageQueueAdapter::NEW)),
            last: AtomicUsize::new(0),
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

/// The cache fictitious pages come from: pages outside the page array, such
/// as placeholders.
static CACHE: Cache<Page> = Cache::new("vm_page");

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
            let color = (frame ^ (frame / COLORS) ^ (frame / (COLORS * COLORS))) % COLORS;
            QUEUES.free[color].push(&array.pages[frame - first]);
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

/// The page holding `address`; `None` if the page array does not cover it.
///
/// # Panics
///
/// If the page array is not set up.
pub fn lookup(address: PhysAddr) -> Option<&'static Page> {
    let array = PAGES.get().expect("vm_page: not set up");
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
/// hint, up to all; `None` when no page is free. `index` is the page's index
/// in its object plus the object's colour.
pub fn allocate(index: usize) -> Option<&'static Page> {
    let cpu_colors = COLORS / machine::processor_count();
    let first = smp::current().id() * cpu_colors + index % cpu_colors;
    let queue = &QUEUES.free[first];
    if let Some(page) = queue.pop() {
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
            if let Some(page) = QUEUES.free[color].pop() {
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
    None
}

/// Takes a free page as `allocate` does for `index`, and fills it with
/// zeros; `None` when no page is free.
pub fn allocate_zeroed(index: usize) -> Option<&'static Page> {
    let page = allocate(index)?;
    // SAFETY: the page is free, so the kernel's alone, and the direct map
    // reaches all of it.
    unsafe {
        page.address()
            .direct_map_ptr::<u8>()
            .write_bytes(0, PAGE_SIZE);
    }
    Some(page)
}
