// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The slab allocator: caches of objects of one size.

use core::marker::PhantomData;
use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use core::ptr::NonNull;

use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};

use super::Error;
use crate::arch::PAGE_SIZE;
use crate::sync::{CpuLocal, Spinlock};
use crate::vm::page::{self, Page};

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
                slabs: 0,
                objects: 0,
            }),
        }
    }

    /// Adds the cache to the list of caches.
    pub fn register(&'static self) {
        CACHES.lock().push_back(self);
    }

    /// Hands out a buffer from this CPU's slab, taking a new slab when it is
    /// used up.
    fn allocate(&self) -> Result<NonNull<u8>, Error> {
        let (page, offset) = {
            let mut cpu = self.cpus.lock();
            let slab = match cpu.active {
                Some(slab) if slab.carved < self.buffers_per_slab => slab,
                _ => {
                    let page = page::allocate().ok_or(Error::ResourceShortage)?;
                    cpu.slabs += 1;
                    Slab { page, carved: 0 }
                }
            };
            cpu.active = Some(Slab {
                carved: slab.carved + 1,
                ..slab
            });
            cpu.objects += 1;
            drop(cpu);
            (slab.page, slab.carved * self.buffer_size)
        };
        Ok(NonNull::new((page.address() + offset).direct_map_ptr()).expect("slab: buffer at null"))
    }
}

/// One CPU's share of a cache.
#[derive(Clone, Copy)]
struct CpuSlabs {
    /// The slab it hands objects out from.
    active: Option<Slab>,
    /// How many of the cache's slabs it made.
    slabs: usize,
    /// How many of the cache's objects it handed out.
    objects: usize,
}

/// A page of a cache's objects.
#[derive(Clone, Copy)]
struct Slab {
    /// The page.
    page: &'static Page,
    /// How many of its objects were handed out first, from its start.
    carved: usize,
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
    marker: PhantomData<T>,
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
        Ok(CacheBox { object })
    }
}

/// An object from a cache, which it owns.
pub struct CacheBox<T> {
    object: NonNull<T>,
}

impl<T> CacheBox<T> {
    /// Gives the object up for good, to a structure that holds objects by
    /// pointer.
    pub fn into_raw(this: Self) -> NonNull<T> {
        ManuallyDrop::new(this).object
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
