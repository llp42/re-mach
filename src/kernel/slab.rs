// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The slab allocator: caches of objects of one size.

use core::marker::PhantomData;

use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};

use crate::arch::PAGE_SIZE;
use crate::sync::Spinlock;

/// The smallest alignment of a cache's objects: a free one holds a link to
/// the next.
const MIN_ALIGN: usize = 8;

/// A cache's untyped state.
#[expect(dead_code, reason = "host_slab_info and kmem_cache_alloc read them")]
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
    /// How many objects a slab of one page holds.
    buffers_per_slab: usize,
}

impl RawCache {
    /// A cache named `name`, of objects of `object_size` bytes aligned to
    /// `align`.
    const fn new(name: &'static str, object_size: usize, align: usize) -> Self {
        assert!(object_size > 0, "slab: empty objects");
        let align = if align < MIN_ALIGN { MIN_ALIGN } else { align };
        let buffer_size = object_size.next_multiple_of(align);
        assert!(buffer_size <= PAGE_SIZE, "slab: objects larger than a page");
        Self {
            list_link: LinkedListAtomicLink::new(),
            name,
            object_size,
            align,
            buffer_size,
            buffers_per_slab: PAGE_SIZE / buffer_size,
        }
    }
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
    /// A cache named `name`, laid out for `T`.
    pub const fn new(name: &'static str) -> Self {
        Self {
            raw: RawCache::new(name, size_of::<T>(), align_of::<T>()),
            marker: PhantomData,
        }
    }

    /// Adds the cache to the list of caches.
    pub fn register(&'static self) {
        CACHES.lock().push_back(&self.raw);
    }
}

/// Every cache, for `host_slab_info` and reclaiming memory.
static CACHES: Spinlock<LinkedList<CacheAdapter>> =
    Spinlock::new(LinkedList::new(CacheAdapter::NEW));
