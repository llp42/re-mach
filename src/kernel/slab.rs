// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The slab allocator: caches of objects of one size.

use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};

/// A cache's untyped state; so far only its place in the list of caches.
pub struct RawCache {
    /// Its place in the list of caches.
    list_link: LinkedListAtomicLink,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    CacheAdapter = &'static RawCache: RawCache { list_link => LinkedListAtomicLink }
);

/// Every cache, for `host_slab_info` and reclaiming memory.
#[expect(
    dead_code,
    reason = "kmem_cache_init adds to it and host_slab_info reads it"
)]
static CACHES: LinkedList<CacheAdapter> = LinkedList::new(CacheAdapter::NEW);
