// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! General-purpose kernel memory, for sizes no typed cache serves.

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
