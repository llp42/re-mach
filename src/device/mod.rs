// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Devices; so far only the master device port, the device server's map
//! and caches, device lookup, network input, the device pager, terminals'
//! input settings and the interrupt thread.

pub mod interrupt;
mod lookup;
mod net;
mod pager;
mod terminal;

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::arch::PAGE_SIZE;
use crate::ipc::port::Port;
use crate::slab::{CacheBox, MIN_ALIGN, RawCache};
use crate::sync::OnceLock;
use crate::vm;
use crate::vm::map::Map;

/// The master device port, which devices are opened through; once
/// `create_service` makes it.
pub static MASTER_PORT: AtomicPtr<Port> = AtomicPtr::new(ptr::null_mut());

/// The kernel's map for device I/O data, 16 MiB; once `create_service`
/// makes it.
static IO_MAP: OnceLock<Map> = OnceLock::new();

/// The cache in-band I/O data comes from, 128 bytes each.
static INBAND_CACHE: RawCache = RawCache::new("io_buf_ptr_inband", 128, MIN_ALIGN, PAGE_SIZE);

/// The cache device traps' I/O requests come from.
static TRAP_REQUEST_CACHE: RawCache = RawCache::new("io_req", 2048, MIN_ALIGN, PAGE_SIZE);

/// Creates the device service, in the kernel task: so far only the master
/// device port, a port the kernel receives on that stands for nothing, then
/// the device server's I/O map, which waits for space, and its caches, then
/// device lookup, network input, the device pager and terminals' input
/// settings.
///
/// # Panics
///
/// If the port can't be allocated, or the kernel map has no room for the
/// I/O map.
pub fn create_service() {
    let port = Port::allocate_kernel()
        .unwrap_or_else(|error| panic!("can't allocate master device port: {error:?}"));
    MASTER_PORT.store(CacheBox::into_raw(port).as_ptr(), Ordering::Release);
    vm::kernel::submap(&IO_MAP, 16 << 20);
    IO_MAP.get().unwrap().set_wait_for_space();
    INBAND_CACHE.register();
    TRAP_REQUEST_CACHE.register();
    lookup::init();
    net::init();
    pager::init();
    terminal::init();
}
