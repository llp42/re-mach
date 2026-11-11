// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Device lookup, by name, number or port; so far only the cache devices
//! come from.

use crate::arch::PAGE_SIZE;
use crate::slab::{MIN_ALIGN, RawCache};

/// The cache devices come from, 80 bytes each.
static CACHE: RawCache = RawCache::new("mach_device", 80, MIN_ALIGN, PAGE_SIZE);

/// Sets up device lookup, on the boot CPU.
pub fn init() {
    CACHE.register();
}
