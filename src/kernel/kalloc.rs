// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! General-purpose kernel memory, for sizes no typed cache serves.

use core::ptr::NonNull;

use super::Error;
use super::slab::{MIN_ALIGN, RawCache};

/// The size of a kalloc cache's slabs, each aligned to it.
pub const SLAB_SIZE: usize = 128 << 10;

/// The kalloc caches, from 32 bytes to 16 KiB, a power of two each; larger
/// sizes go straight to the kernel map.
static CACHES: [RawCache; 10] = [
    RawCache::new("kalloc_32", 32, MIN_ALIGN, SLAB_SIZE),
    RawCache::new("kalloc_64", 64, MIN_ALIGN, SLAB_SIZE),
    RawCache::new("kalloc_128", 128, MIN_ALIGN, SLAB_SIZE),
    RawCache::new("kalloc_256", 256, MIN_ALIGN, SLAB_SIZE),
    RawCache::new("kalloc_512", 512, MIN_ALIGN, SLAB_SIZE),
    RawCache::new("kalloc_1024", 1024, MIN_ALIGN, SLAB_SIZE),
    RawCache::new("kalloc_2048", 2048, MIN_ALIGN, SLAB_SIZE),
    RawCache::new("kalloc_4096", 4096, MIN_ALIGN, SLAB_SIZE),
    RawCache::new("kalloc_8192", 8192, MIN_ALIGN, SLAB_SIZE),
    RawCache::new("kalloc_16384", 16384, MIN_ALIGN, SLAB_SIZE),
];

/// Sets up kalloc, on the boot CPU.
pub fn init() {
    for cache in &CACHES {
        cache.register();
    }
}

/// Takes a buffer of at least `size` bytes from the smallest cache that
/// holds it.
///
/// # Errors
///
/// As `RawCache::allocate`.
///
/// # Panics
///
/// If `size` is 0, or above 16 KiB, which the kernel map serves: not yet.
pub fn allocate(size: usize) -> Result<NonNull<u8>, Error> {
    assert!(size != 0, "kalloc: 0 bytes, not yet");
    // Each cache's objects are 32 << its index bytes.
    let index = (0..CACHES.len())
        .find(|index| 32 << index >= size)
        .unwrap_or_else(|| panic!("kalloc: {size} bytes, above the caches, not yet"));
    CACHES[index].allocate()
}
