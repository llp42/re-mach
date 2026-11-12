// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The device pager, which backs memory objects mapped from devices; so
//! far only its caches.

use crate::arch::PAGE_SIZE;
use crate::slab::{MIN_ALIGN, RawCache};

/// The cache device pagers come from, 72 bytes each.
static CACHE: RawCache = RawCache::new("dev_pager", 72, MIN_ALIGN, PAGE_SIZE);

/// The cache entries of the pagers' hash by name port come from, 32 bytes
/// each.
static PAGER_ENTRY_CACHE: RawCache = RawCache::new("dev_pager_entry", 32, MIN_ALIGN, PAGE_SIZE);

/// The cache entries of the pagers' hash by device and offset come from,
/// 40 bytes each.
static DEVICE_ENTRY_CACHE: RawCache = RawCache::new("dev_device_entry", 40, MIN_ALIGN, PAGE_SIZE);

/// Sets up the device pager, on the boot CPU.
pub fn init() {
    CACHE.register();
    PAGER_ENTRY_CACHE.register();
    DEVICE_ENTRY_CACHE.register();
}
