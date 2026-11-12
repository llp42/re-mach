// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Network input: packet filters, which deliver packets to ports; so far
//! only their caches.

use crate::arch::PAGE_SIZE;
use crate::slab::{MIN_ALIGN, RawCache};

/// The cache filters come from, each with the port it delivers to, 1088
/// bytes each.
static RECEIVE_PORT_CACHE: RawCache = RawCache::new("net_rcv_port", 1088, MIN_ALIGN, PAGE_SIZE);

/// The cache the filters' hash entries come from, 48 bytes each.
static HASH_ENTRY_CACHE: RawCache = RawCache::new("net_hash_entry", 48, MIN_ALIGN, PAGE_SIZE);

/// Sets up network input, on the boot CPU.
pub fn init() {
    RECEIVE_PORT_CACHE.register();
    HASH_ENTRY_CACHE.register();
}
