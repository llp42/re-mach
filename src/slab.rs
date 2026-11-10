// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The slab allocator: caches of objects of one size.

use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU64, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};

use crate::arch::{PAGE_SIZE, PhysAddr};
use crate::error::Error;
use crate::sync::{CpuLocal, Spinlock};
use crate::vm;
use crate::vm::page::{self, Page, PageQueueAdapter};

/// The smallest alignment of a cache's objects: a free one holds a link to
/// the next.
pub const MIN_ALIGN: usize = 8;

/// A cache's untyped state.
#[expect(dead_code, reason = "host_slab_info reads them")]
pub struct RawCache {
    /// Its place in the list of caches.
    list_link: LinkedListAtomicLink,
    /// Its name, as `host_slab_info` reports it.
    name: &'static str,
    /// The size its users ask for.
    object_size: usize,
    /// Each object's alignment, at least `MIN_ALIGN`.
    align: usize,
    /// The space each object takes in a slab.
    buffer_size: usize,
    /// The size of its slabs.
    slab_size: usize,
    /// How many objects a slab holds.
    buffers_per_slab: usize,
    /// Each CPU's slabs.
    cpus: CpuLocal<CpuSlabs>,
    /// The slabs no CPU hands objects out from.
    lists: Spinlock<SlabLists>,
}

impl RawCache {
    /// A cache named `name`, of objects of `object_size` bytes aligned to
    /// `align`, on slabs of `slab_size` bytes.
    pub const fn new(
        name: &'static str,
        object_size: usize,
        align: usize,
        slab_size: usize,
    ) -> Self {
        assert!(object_size > 0, "slab: empty objects");
        let align = if align < MIN_ALIGN { MIN_ALIGN } else { align };
        let buffer_size = object_size.next_multiple_of(align);
        assert!(buffer_size <= slab_size, "slab: objects larger than a slab");
        Self {
            list_link: LinkedListAtomicLink::new(),
            name,
            object_size,
            align,
            buffer_size,
            slab_size,
            buffers_per_slab: slab_size / buffer_size,
            cpus: CpuLocal::new(CpuSlabs {
                active: None,
                carved: 0,
                taken: Chain { head: 0, count: 0 },
                slabs: 0,
                objects: 0,
            }),
            lists: Spinlock::new(SlabLists {
                partial: LinkedList::new(PageQueueAdapter::NEW),
                free: LinkedList::new(PageQueueAdapter::NEW),
                exhausted: LinkedList::new(PageQueueAdapter::NEW),
            }),
        }
    }

    /// Adds the cache to the list of caches.
    pub fn register(&'static self) {
        CACHES.lock().push_back(self);
    }

    /// Hands out a buffer from this CPU's slab: the next of those it took
    /// from the slab's free list, else the first of that list, taken whole,
    /// else the next uncarved one. A used-up slab goes on the exhausted list
    /// and gives way to a partial one, else a free one, else, once up to 16
    /// exhausted ones are sorted, one of those, else a new one: a page, or a
    /// zone of the kernel map for larger slabs, made with the slot released,
    /// put on the free list, then taken from it.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if it needs a new slab and no page or entry can be
    /// allocated, and `NoSpace` if it needs a new zone and the kernel map
    /// has no room for one.
    pub fn allocate(&self) -> Result<NonNull<u8>, Error> {
        loop {
            let mut cpu = self.cpus.lock();
            if cpu.taken.count == 0
                && cpu.active.is_none_or(|page| {
                    cpu.carved == self.buffers_per_slab && page.slab.free.count() == 0
                })
            {
                // The lists' lock is taken inside the CPU's slot.
                let mut lists = self.lists.lock();
                if let Some(page) = cpu.active.take() {
                    lists.exhausted.push_back(page);
                }
                if lists.partial.is_empty() && lists.free.is_empty() {
                    // Sorts up to 16; those left exhausted go to the end.
                    let mut left = LinkedList::new(PageQueueAdapter::NEW);
                    for _ in 0..16 {
                        let Some(page) = lists.exhausted.pop_front() else {
                            break;
                        };
                        let freed = page.slab.free.count();
                        match freed {
                            0 => left.push_back(page),
                            _ if freed == self.buffers_per_slab => lists.free.push_back(page),
                            _ => lists.partial.push_back(page),
                        }
                    }
                    lists.exhausted.back_mut().splice_after(left);
                }
                if let Some(page) = lists.partial.pop_front().or_else(|| lists.free.pop_front()) {
                    // All carved; while listed only frees change its free word, so it
                    // is not 0.
                    cpu.active = Some(page);
                    cpu.carved = self.buffers_per_slab;
                }
                drop(lists);
            }
            if cpu.taken.count == 0
                && let Some(page) = cpu.active
                && page.slab.free.count() != 0
            {
                cpu.taken = page.slab.free.take();
            }
            let (page, offset) = match cpu.active {
                Some(page) if cpu.taken.count != 0 => {
                    let offset = usize::try_from(cpu.taken.head - 1).unwrap();
                    // SAFETY: a taken buffer is free and this CPU's alone, and
                    // starts with the link to the next.
                    cpu.taken.head = unsafe { self.buffer::<u64>(page, offset).read() };
                    cpu.taken.count -= 1;
                    (page, offset)
                }
                Some(page) if cpu.carved < self.buffers_per_slab => {
                    let carved = cpu.carved;
                    cpu.carved += 1;
                    (page, carved * self.buffer_size)
                }
                _ if self.slab_size != PAGE_SIZE => {
                    // A zone, made with the slot released, goes on the free
                    // list.
                    drop(cpu);
                    let page = self.grow()?;
                    let mut cpu = self.cpus.lock();
                    cpu.slabs += 1;
                    self.lists.lock().free.push_back(page);
                    drop(cpu);
                    continue;
                }
                _ => {
                    // A CPU's slabs index their pages, at colour 0.
                    let page = page::allocate(cpu.slabs).ok_or(Error::ResourceShortage)?;
                    // A new slab has nothing freed to it.
                    page.slab.free.take();
                    cpu.slabs += 1;
                    cpu.active = Some(page);
                    cpu.carved = 1;
                    (page, 0)
                }
            };
            cpu.objects += 1;
            drop(cpu);
            return Ok(NonNull::new(self.buffer(page, offset)).expect("slab: buffer at null"));
        }
    }

    /// Makes a slab larger than a page: a zone of the kernel map, aligned to
    /// its size, its first page holding its state, with every buffer on its
    /// free list, in address order.
    ///
    /// # Errors
    ///
    /// As `vm::kernel::allocate_aligned`.
    fn grow(&self) -> Result<&'static Page, Error> {
        let start = vm::kernel::allocate_aligned(self.slab_size)?;
        let page =
            vm::kernel::page_of(start.addr().get()).expect("slab: zone outside the page array");
        let end = self.buffers_per_slab * self.buffer_size;
        // A new slab has nothing freed to it.
        page.slab.free.take();
        // Pushed from the last, so the list starts at the first.
        for offset in (0..end).step_by(self.buffer_size).rev() {
            // SAFETY: the zone is this slab's alone, its buffer at `offset`
            // inside it.
            unsafe { page.slab.free.push(start.add(offset), offset) };
        }
        Ok(page)
    }

    /// Where the buffer at `offset` in the slab `page` starts: in the direct
    /// map for a slab of a page, else in the kernel map.
    fn buffer<T>(&self, page: &'static Page, offset: usize) -> *mut T {
        if self.slab_size == PAGE_SIZE {
            (page.address() + offset).direct_map_ptr()
        } else {
            ptr::with_exposed_provenance_mut(vm::kernel::address_of(page) + offset)
        }
    }

    /// The slab `buffer` is in, by its page, and its offset there: the
    /// inverse of `buffer`. A slab of a page is found through the direct
    /// map, a zone by masking with its size.
    ///
    /// # Panics
    ///
    /// If the page array does not cover `buffer`.
    fn slab_of(&self, buffer: NonNull<u8>) -> (&'static Page, usize) {
        if self.slab_size == PAGE_SIZE {
            let address = PhysAddr::from_direct_map(buffer.as_ptr());
            let page = page::lookup(address).expect("slab: buffer outside the page array");
            (page, address - page.address())
        } else {
            // A zone is aligned to its size, and its first page holds its
            // state.
            let start = buffer.addr().get() & !(self.slab_size - 1);
            let page = vm::kernel::page_of(start).expect("slab: zone outside the page array");
            (page, buffer.addr().get() - start)
        }
    }

    /// Gives `buffer` back to its slab's free list, from any CPU, with a
    /// lock-free push.
    ///
    /// # Safety
    ///
    /// `buffer` came from this cache, and is not used again.
    ///
    /// # Panics
    ///
    /// If the page array does not cover `buffer`.
    pub unsafe fn free(&self, buffer: NonNull<u8>) {
        let (page, offset) = self.slab_of(buffer);
        // SAFETY: the buffer is the caller's to give up, its slab's at
        // `offset`.
        unsafe { page.slab.free.push(buffer, offset) };
        self.cpus.lock().objects -= 1;
    }
}

/// A slab's state, in its page.
pub struct Slab {
    /// The buffers freed to it.
    free: FreeList,
}

impl Slab {
    /// One with no buffers freed to it.
    pub const fn new() -> Self {
        Self {
            free: FreeList::new(),
        }
    }
}

/// A slab's free buffers, pushed from any CPU and taken whole: in one word,
/// the first one's offset in the slab plus one, 0 for none, in the low 32
/// bits, and how many in the high 32. Each starts with the next one's
/// offset plus one, as a `u64`.
struct FreeList(AtomicU64);

impl FreeList {
    /// An empty one.
    const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// Adds `buffer`, at `offset` in the slab, with a lock-free push.
    ///
    /// # Safety
    ///
    /// `buffer` is the slab's at `offset`, free, and no one else's.
    unsafe fn push(&self, buffer: NonNull<u8>, offset: usize) {
        let head = u64::try_from(offset + 1).unwrap();
        let mut word = self.0.load(Ordering::Relaxed);
        loop {
            // SAFETY: the buffer is the caller's, and at least 8 bytes,
            // 8-aligned.
            unsafe { buffer.cast::<u64>().write(word & 0xffff_ffff) };
            // Release, so the take sees the link. Pushes only add and a take
            // empties the list, so a word that compares equal is the list as
            // it stands: no ABA.
            match self.0.compare_exchange_weak(
                word,
                (((word >> 32) + 1) << 32) | head,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(current) => word = current,
            }
        }
    }

    /// Takes every buffer, leaving it empty.
    fn take(&self) -> Chain {
        // Acquire, to see the links the pushes wrote.
        let word = self.0.swap(0, Ordering::Acquire);
        Chain {
            head: word & 0xffff_ffff,
            count: usize::try_from(word >> 32).unwrap(),
        }
    }

    /// How many buffers it holds.
    fn count(&self) -> usize {
        usize::try_from(self.0.load(Ordering::Relaxed) >> 32).unwrap()
    }
}

/// Buffers taken from a free list, linked as there.
#[derive(Clone, Copy)]
struct Chain {
    /// The first one's offset in the slab plus one, 0 for none.
    head: u64,
    /// How many.
    count: usize,
}

/// A cache's slabs no CPU hands objects out from, each carved whole.
struct SlabLists {
    /// Those with some objects free.
    partial: LinkedList<PageQueueAdapter>,
    /// Those with every object free.
    free: LinkedList<PageQueueAdapter>,
    /// Those with no object free when last looked at; frees since are found
    /// by sorting them.
    exhausted: LinkedList<PageQueueAdapter>,
}

/// One CPU's share of a cache.
#[derive(Clone, Copy)]
struct CpuSlabs {
    /// The slab it hands objects out from.
    active: Option<&'static Page>,
    /// How many of the active slab's objects were handed out, from its start.
    carved: usize,
    /// The buffers it took from the active slab's free list and has not
    /// handed out.
    taken: Chain,
    /// How many of the cache's slabs it made.
    slabs: usize,
    /// How many of the cache's objects it handed out, less those freed on
    /// it; a free can come from a CPU that didn't hand the object out.
    objects: isize,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    CacheAdapter = &'static RawCache: RawCache { list_link => LinkedListAtomicLink }
);

/// A cache of objects of type `T`.
pub struct Cache<T> {
    raw: RawCache,
    /// The cache holds no `T`, so it is shared whatever `T` is.
    marker: PhantomData<fn() -> T>,
}

impl<T> Cache<T> {
    /// A cache named `name`, laid out for `T`, on slabs of one page.
    pub const fn new(name: &'static str) -> Self {
        assert!(
            size_of::<T>() <= PAGE_SIZE,
            "slab: objects larger than a page"
        );
        Self {
            raw: RawCache::new(name, size_of::<T>(), align_of::<T>(), PAGE_SIZE),
            marker: PhantomData,
        }
    }

    /// Adds the cache to the list of caches.
    pub fn register(&'static self) {
        self.raw.register();
    }

    /// Puts `value` in a new object from the cache.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the cache needs a new slab and no page is free.
    pub fn allocate(&'static self, value: T) -> Result<CacheBox<T>, Error> {
        let object = self.raw.allocate()?.cast::<T>();
        // SAFETY: the buffer is this object's alone, and aligned and large
        // enough for a `T`.
        unsafe { object.write(value) };
        Ok(CacheBox {
            cache: self,
            object,
        })
    }
}

/// An object from a cache, which it owns, and frees to the cache when
/// dropped.
pub struct CacheBox<T: 'static> {
    /// The cache it came from.
    cache: &'static Cache<T>,
    object: NonNull<T>,
}

impl<T> CacheBox<T> {
    /// Gives the object up, to a structure that holds objects by pointer;
    /// `from_raw` takes it back.
    pub fn into_raw(this: Self) -> NonNull<T> {
        ManuallyDrop::new(this).object
    }

    /// Takes back an object `into_raw` gave up.
    ///
    /// # Safety
    ///
    /// `object` came from `into_raw` of a box of `cache`, and nothing but
    /// the new box uses it after.
    pub const unsafe fn from_raw(cache: &'static Cache<T>, object: NonNull<T>) -> Self {
        Self { cache, object }
    }
}

impl<T> Drop for CacheBox<T> {
    fn drop(&mut self) {
        // SAFETY: the box owns the object, which nothing uses after, and its
        // buffer came from the cache.
        unsafe {
            self.object.drop_in_place();
            self.cache.raw.free(self.object.cast());
        }
    }
}

impl<T> Deref for CacheBox<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the object is initialized and this box owns it.
        unsafe { self.object.as_ref() }
    }
}

impl<T> DerefMut for CacheBox<T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the object is initialized and this box owns it.
        unsafe { self.object.as_mut() }
    }
}

/// Every cache, for `host_slab_info` and reclaiming memory.
static CACHES: Spinlock<LinkedList<CacheAdapter>> =
    Spinlock::new(LinkedList::new(CacheAdapter::NEW));
