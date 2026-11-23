// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The debugging calls' server routines, as MIG makes them from
//! `mach_debug`'s definitions; only `host_slab_info` so far.

use core::mem::size_of;
use core::ptr;
use core::slice;

use super::mig;
use super::mig::argument::{Element, OutOfLine, word};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::host::Host;
use crate::ipc::KERNEL_MAP;
use crate::ipc::MessageType;
use crate::ipc::port::port::Port;
use crate::slab::{self, CacheInfo};
use crate::vm;
use crate::vm::Protection;
use crate::vm::map::change::{Placement, RangeCheck, Source};

mig::subsystem! {
    base 3000;
    22 host_slab_info(host = Port::as_host, count: u32) -> (info: OutOfLine<CacheInfo>);
}

/// Mach's `cache_info_t`, as MIG carries it: fifteen 64-bit words, long
/// form, out of line.
// SAFETY: `repr(C)`, aligned to 8, every byte a field, including the
// explicit padding.
unsafe impl Element for CacheInfo {
    const NAME: MessageType = MessageType::Integer64;
    const SIZE: u16 = 64;

    fn out_of_line_word(number: u32) -> u64 {
        // The long-form bit, bit 30; fifteen words per cache.
        word(Self::NAME, Self::SIZE, 1 << 6, number * 15)
    }
}

/// Serves `host_slab_info`: every cache's information, as
/// `cache_info_t` gives it, out of line.
///
/// # Errors
///
/// `InvalidHost` for no host.
///
/// # Panics
///
/// If no cache is registered, if the kernel map has no room, or if a cache
/// registers while the information is gathered: not yet.
fn host_slab_info(host: Option<&&Host>, _count: u32) -> Result<OutOfLine<CacheInfo>, Error> {
    if host.is_none() {
        return Err(Error::InvalidHost);
    }
    let count = slab::cache_count();
    assert!(count != 0, "host_slab_info: no caches, not yet");
    let size = (count * size_of::<CacheInfo>()).next_multiple_of(PAGE_SIZE);
    let map = KERNEL_MAP.get().expect("host_slab_info: no ipc_kernel_map");
    let start = vm::user::allocate(map, Placement::Anywhere, size)
        .unwrap_or_else(|error| panic!("host_slab_info: vm_allocate failed: {error:?}, not yet"));
    let range = start..start + size;
    if let Err(error) = map.pageable(
        range.clone(),
        Protection::READ | Protection::WRITE,
        RangeCheck::Checked,
    ) {
        panic!("host_slab_info: vm_map_pageable failed: {error:?}");
    }
    // SAFETY: the allocation is `size` bytes of the IPC map, page-aligned
    // and wired here; nothing else reads or writes it.
    let infos = unsafe {
        slice::from_raw_parts_mut(
            ptr::with_exposed_provenance_mut::<CacheInfo>(usize::from(start)),
            size / size_of::<CacheInfo>(),
        )
    };
    let (written, total) = slab::cache_info(&mut infos[..count]);
    assert!(
        written == total,
        "host_slab_info: a cache registered, not yet"
    );
    if let Err(error) = map.pageable(range.clone(), Protection::empty(), RangeCheck::Checked) {
        panic!("host_slab_info: vm_map_pageable failed: {error:?}");
    }
    let copy = map.copy_in(range, Source::Destroy);
    Ok(OutOfLine::new(copy, count))
}
