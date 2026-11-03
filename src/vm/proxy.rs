// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Proxy memory objects: ports that map part of a memory object, allowing
//! at most a given protection.

use core::ops::Range;

use super::Protection;
use crate::kernel::slab::Cache;

/// A proxy memory object; so far only what it allows and the part it maps.
#[expect(dead_code, reason = "memory_object_create_proxy and vm_map use them")]
pub struct Proxy {
    /// The most a mapping of it may allow.
    max_protection: Protection,
    /// The part of the memory object it maps, by offset.
    range: Range<usize>,
}

/// The cache proxies come from.
static CACHE: Cache<Proxy> = Cache::new("memory_object_proxy");

/// Sets up proxy memory objects, on the boot CPU.
pub fn init() {
    CACHE.register();
}
