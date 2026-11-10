// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Physical pages.

use core::mem::MaybeUninit;
use core::ops::Range;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicU16, AtomicU32, AtomicUsize, Ordering};
use core::{ptr, slice};

use intrusive_collections::{
    KeyAdapter, LinkedList, LinkedListAtomicLink, RBTreeAtomicLink, intrusive_adapter,
};

use super::object::Object;
use super::phys_map::MachinePhysMap as _;
use crate::arch::{self, PAGE_SIZE, PhysAddr};
use crate::machine;
use crate::slab::{Cache, Slab};
use crate::sync::{BootCell, CachePadded, OnceLock, Spinlock};
use crate::{println, smp};

/// A physical page.
pub struct Page {
    /// Its place in the page queue it is on, or in its cache's list while it
    /// is a slab.
    queue_link: LinkedListAtomicLink,
    /// Its place in its object's tree, while it belongs to one.
    object_link: RBTreeAtomicLink,
    /// The object it belongs to; null while it belongs to none. Changed
    /// only behind the object's token.
    object: AtomicPtr<Object>,
    /// Where it is in its object, in bytes; changed as `object` is.
    offset: AtomicUsize,
    /// Its busy word: `HARD_BUSY` while the thread that set it fills or
    /// maps it, and others keep off it.
    busy: AtomicU32,
    /// How many times it is wired, which keeps it off the active and
    /// inactive queues; changed only behind its object's token.
    wire_count: AtomicU16,
    /// Its `Flags`; changed only behind its object's token.
    flags: AtomicU8,
    /// Its lock while it is a user page table, over its entries.
    pub table_lock: Spinlock<()>,
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
            busy: AtomicU32::new(0),
            wire_count: AtomicU16::new(0),
            flags: AtomicU8::new(0),
            table_lock: Spinlock::new(()),
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

    /// Where it is in its object, in bytes.
    pub(super) fn offset(&self) -> usize {
        self.offset.load(Ordering::Relaxed)
    }

    /// Makes it busy, before it goes into an object.
    pub(super) fn set_busy(&self) {
        self.busy.fetch_or(HARD_BUSY, Ordering::Relaxed);
    }

    /// Whether it is busy, behind its object's token.
    pub(super) fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed) & HARD_BUSY != 0
    }

    /// Makes it no longer busy and, once anything waits, wakes the waiters,
    /// behind its object's token.
    pub(super) fn clear_busy(&self) {
        // Nothing waits on a busy page yet, so none is woken.
        self.busy.fetch_and(!HARD_BUSY, Ordering::Relaxed);
    }

    /// Whether it is on the active or the inactive queue, behind its
    /// object's token.
    pub(super) fn is_on_paging_queue(&self) -> bool {
        Flags::from_bits_retain(self.flags.load(Ordering::Relaxed))
            .intersects(Flags::ACTIVE | Flags::INACTIVE)
    }

    /// Notes that it was used, behind its object's token.
    pub(super) fn set_referenced(&self) {
        self.flags
            .fetch_or(Flags::REFERENCED.bits(), Ordering::Relaxed);
    }

    /// Puts it, referenced, at the end of its colour's active queue, behind
    /// its object's token.
    ///
    /// # Panics
    ///
    /// If it is on a paging queue or wired: not yet.
    pub(super) fn activate(&'static self) {
        let flags = Flags::from_bits_retain(self.flags.load(Ordering::Relaxed));
        assert!(
            !flags.intersects(Flags::ACTIVE | Flags::INACTIVE),
            "vm_page_activate: a page on a paging queue, not yet"
        );
        assert!(
            self.wire_count.load(Ordering::Relaxed) == 0,
            "vm_page_activate: a wired page, not yet"
        );
        let color = color(self.address().0 / PAGE_SIZE);
        QUEUES.active.push(color, self);
        self.flags.store(
            (flags | Flags::ACTIVE | Flags::REFERENCED).bits(),
            Ordering::Relaxed,
        );
    }

    /// Takes it off the active queue, unreferenced, and puts it at the end of
    /// its colour's inactive queue unless it is wired or there already,
    /// behind its object's token.
    ///
    /// # Panics
    ///
    /// If it is inactive and referenced: not yet.
    pub(super) fn deactivate(&'static self) {
        let mut flags = Flags::from_bits_retain(self.flags.load(Ordering::Relaxed));
        assert!(
            !flags.contains(Flags::INACTIVE | Flags::REFERENCED),
            "vm_page_deactivate: a referenced inactive page, not yet"
        );
        let color = color(self.address().0 / PAGE_SIZE);
        if flags.contains(Flags::ACTIVE) {
            // pmap_clear_reference has no counterpart until pageout reads
            // accessed bits.
            flags.remove(Flags::ACTIVE | Flags::REFERENCED);
            // SAFETY: an active page is on its colour's active queue.
            unsafe { QUEUES.active.remove(color, self) };
        }
        if self.wire_count.load(Ordering::Relaxed) == 0 && !flags.contains(Flags::INACTIVE) {
            QUEUES.inactive.push(color, self);
            flags.insert(Flags::INACTIVE);
        }
        self.flags.store(flags.bits(), Ordering::Relaxed);
    }

    /// Wires it once more, behind its object's token.
    ///
    /// # Panics
    ///
    /// If it is unwired and on a paging queue: not yet.
    pub(super) fn wire(&self) {
        let count = self.wire_count.load(Ordering::Relaxed);
        if count == 0 {
            assert!(
                !self.is_on_paging_queue(),
                "vm_page_wire: a page on a paging queue, not yet"
            );
            QUEUES.wired_pages.fetch_add(1, Ordering::Relaxed);
        }
        self.wire_count.store(count + 1, Ordering::Relaxed);
    }

    /// Fills it with zeros.
    ///
    /// # Safety
    ///
    /// Nothing else may use its memory meanwhile.
    pub(super) unsafe fn zero_fill(&'static self) {
        // SAFETY: the caller has the page to itself, and the direct map
        // reaches all of it.
        unsafe {
            self.address()
                .direct_map_ptr::<u8>()
                .write_bytes(0, PAGE_SIZE);
        }
    }

    /// Where the page is.
    ///
    /// # Panics
    ///
    /// If the page array is not set up.
    pub fn address(&'static self) -> PhysAddr {
        let array = pages();
        let index = (ptr::from_ref(self).addr() - array.pages.as_ptr().addr()) / size_of::<Self>();
        array.start + index * PAGE_SIZE
    }
}

/// The busy word's bit for a page one thread has to itself.
const HARD_BUSY: u32 = 1 << 31;

bitflags::bitflags! {
    /// A page's paging queue, and whether it was used.
    struct Flags: u8 {
        /// On its colour's active queue.
        const ACTIVE = 1;
        /// On its colour's inactive queue.
        const INACTIVE = 1 << 1;
        /// Used since it was last deactivated.
        const REFERENCED = 1 << 2;
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
    /// The DMA reserve's pool.
    reserve: Pool,
    /// The colour queues' pool: the pages above the reserve.
    queues: Pool,
}

/// A pool's pages and its free-page thresholds.
struct Pool {
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
    fn new(pages: usize) -> Self {
        Self {
            pages,
            min: (pages * 5 / 100).max(500),
            low: (pages * 6 / 100).max(600),
            high: (pages * 10 / 100).max(1000),
        }
    }
}

/// The page array, once `setup` builds it.
static PAGES: OnceLock<PageArray> = OnceLock::new();

/// The page array.
///
/// # Panics
///
/// If it is not set up.
#[track_caller]
fn pages() -> &'static PageArray {
    PAGES.get().expect("vm_page: not set up")
}

/// How many colours each page queue has; a page's colour is the exclusive
/// or of its frame number's three lowest digits in base `COLORS`.
const COLORS: usize = 1024;

/// The colour of frame number `frame`.
const fn color(frame: usize) -> usize {
    (frame ^ (frame / COLORS) ^ (frame / (COLORS * COLORS))) % COLORS
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub PageQueueAdapter = &'static Page: Page { queue_link => LinkedListAtomicLink }
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

    /// Puts `page` at its end, counting it in its pool's `total` too.
    fn push(&self, page: &'static Page, total: &AtomicUsize) {
        let mut list = self.list.lock();
        list.push_back(page);
        // The count changes only under the lock.
        self.pages
            .store(self.pages.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
        total.fetch_add(1, Ordering::Relaxed);
        drop(list);
    }

    /// Takes `page` off it, dropping it from its pool's `total` too.
    ///
    /// # Safety
    ///
    /// `page` must be on it.
    unsafe fn remove(&self, page: &'static Page, total: &AtomicUsize) {
        let mut list = self.list.lock();
        // SAFETY: the caller's.
        unsafe { list.cursor_mut_from_ptr(page).remove() };
        // The count changes only under the lock.
        self.pages
            .store(self.pages.load(Ordering::Relaxed) - 1, Ordering::Relaxed);
        total.fetch_sub(1, Ordering::Relaxed);
        drop(list);
    }

    /// Takes its first page, dropping it from its pool's `total` too; an
    /// empty queue is passed by without its lock.
    fn pop(&self, total: &AtomicUsize) -> Option<&'static Page> {
        if self.pages.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let mut list = self.list.lock();
        let page = list.pop_front()?;
        // The count changes only under the lock.
        self.pages
            .store(self.pages.load(Ordering::Relaxed) - 1, Ordering::Relaxed);
        total.fetch_sub(1, Ordering::Relaxed);
        drop(list);
        Some(page)
    }
}

/// A page queue per colour, each on its own cache lines, and how many
/// pages they hold.
struct ColorQueues {
    /// How many pages its queues hold; changed under the lock of the queue
    /// changed.
    pages: AtomicUsize,
    /// Its queues, by colour.
    queues: [CachePadded<PageQueue>; COLORS],
}

impl ColorQueues {
    /// Empty ones.
    #[expect(
        clippy::large_stack_arrays,
        reason = "only QUEUES' initializer calls it, at compile time"
    )]
    const fn new() -> Self {
        Self {
            pages: AtomicUsize::new(0),
            queues: [const { CachePadded::new(PageQueue::new()) }; COLORS],
        }
    }

    /// Puts `page` at the end of `color`'s queue.
    fn push(&self, color: usize, page: &'static Page) {
        self.queues[color].push(page, &self.pages);
    }

    /// Takes `page` off `color`'s queue.
    ///
    /// # Safety
    ///
    /// `page` must be on it.
    unsafe fn remove(&self, color: usize, page: &'static Page) {
        // SAFETY: the caller's.
        unsafe { self.queues[color].remove(page, &self.pages) };
    }

    /// Takes the first page of `color`'s queue.
    fn pop(&self, color: usize) -> Option<&'static Page> {
        self.queues[color].pop(&self.pages)
    }
}

/// The page queues, by use, and how many pages are wired.
struct PageQueues {
    /// How many pages are wired.
    wired_pages: AtomicUsize,
    /// Free pages.
    free: ColorQueues,
    /// Pages in use, not referenced lately.
    inactive: ColorQueues,
    /// Pages in use, referenced lately.
    active: ColorQueues,
}

/// Most low memory kept for DMA.
const DMA_RESERVE_LIMIT: usize = 256 << 20;

/// The frame the DMA reserve ends before, for a span of `size` bytes.
fn reserve_end(size: usize) -> usize {
    DMA_RESERVE_LIMIT.min(size / 4) / PAGE_SIZE
}

/// The DMA reserve's free frames and their count.
struct Reserve {
    /// A bit per frame from 0, set when it is free.
    frames: [u64; DMA_RESERVE_LIMIT / PAGE_SIZE / 64],
    /// How many bits are set.
    free: usize,
}

/// Free frames kept for DMA, below `DMA_RESERVE_LIMIT`.
static DMA_RESERVE: Spinlock<Reserve> = Spinlock::new(Reserve {
    frames: [0; DMA_RESERVE_LIMIT / PAGE_SIZE / 64],
    free: 0,
});

static QUEUES: PageQueues = PageQueues {
    wired_pages: AtomicUsize::new(0),
    free: ColorQueues::new(),
    inactive: ColorQueues::new(),
    active: ColorQueues::new(),
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
            let bit = 1 << (frame % 64);
            if reserve.frames[frame / 64] & bit == 0 {
                reserve.frames[frame / 64] |= bit;
                reserve.free += 1;
            }
            drop(reserve);
        } else {
            QUEUES.free.push(color(frame), &array.pages[frame - first]);
        }
    }
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

/// Whether every pool is usable for unprivileged allocations: it has no
/// page in use to page out, or at least its high threshold of free pages.
/// The DMA reserve's pages are never paged out.
///
/// # Panics
///
/// If the page array is not set up.
pub(super) fn usable() -> bool {
    let array = pages();
    QUEUES
        .active
        .queues
        .iter()
        .chain(&QUEUES.inactive.queues)
        .all(|queue| queue.pages.load(Ordering::Relaxed) == 0)
        || QUEUES.free.pages.load(Ordering::Relaxed) >= array.queues.high
}

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
/// hint, up to all; `None` when no page is free. `index` is the page's index
/// in its object plus the object's colour.
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
                return None;
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
    None
}

/// Puts `page`, which `allocate` gave and nothing uses now, back on its
/// colour's free queue.
pub fn free(page: &'static Page) {
    QUEUES.free.push(color(page.address().0 / PAGE_SIZE), page);
}

/// Takes a free page as `allocate` does for `index`, and fills it with
/// zeros; `None` when no page is free.
pub fn allocate_zeroed(index: usize) -> Option<&'static Page> {
    let page = allocate(index)?;
    // SAFETY: the page is free, so the kernel's alone.
    unsafe { page.zero_fill() };
    Some(page)
}
