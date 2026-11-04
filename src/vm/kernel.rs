// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Kernel memory: the kernel's map.

use core::ptr::{self, NonNull};

use super::map::Map;
use super::pmap::MachinePmap as _;
use super::{Inheritance, Protection};
use crate::arch::{self, MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE, PhysAddr};
use crate::kernel::Error;
use crate::sync::OnceLock;

/// The kernel's map, once `init` sets it up.
pub static KERNEL_MAP: OnceLock<Map> = OnceLock::new();

/// Sets up the kernel's map, with the boot mappings in use, on the boot CPU.
///
/// # Panics
///
/// If the boot mappings can't be entered, or on a second call.
pub fn init() {
    // A range can't end past the last page, so the map stops before it.
    let bounds = MIN_KERNEL_ADDRESS..MAX_KERNEL_ADDRESS - PAGE_SIZE + 1;
    let map = Map::new(arch::Pmap::kernel(), bounds);
    if let Err(error) = map.enter(
        arch::Pmap::boot_space(),
        Protection::READ | Protection::WRITE,
        Protection::all(),
        Inheritance::Copy,
    ) {
        panic!("kmem_init: vm_map_enter failed: {error:?}");
    }
    assert!(KERNEL_MAP.set(map).is_ok(), "kmem_init: set up twice");
}

/// Makes `submap` a submap of the kernel map of `size` bytes, rounded up to
/// pages, on the boot CPU.
///
/// # Panics
///
/// If the kernel map is not set up or has no room for it, or if `submap`
/// is set already.
pub fn submap(submap: &'static OnceLock<Map>, size: usize) {
    let map = KERNEL_MAP.get().expect("kmem_submap: no kernel map");
    if let Err(error) = map.enter_submap(size.next_multiple_of(PAGE_SIZE), submap) {
        panic!("kmem_submap: vm_map_enter failed: {error:?}");
    }
}

/// Maps `size` bytes of device memory at `physical` into the kernel map,
/// uncached; returns where `physical` lands.
///
/// # Errors
///
/// `NoSpace` if the kernel map has no room for them, and `ResourceShortage`
/// if no entry can be allocated.
///
/// # Panics
///
/// If the kernel map is not set up.
pub fn map_device(physical: PhysAddr, size: usize) -> Result<NonNull<u8>, Error> {
    let map = KERNEL_MAP
        .get()
        .expect("kmem_map_aligned_table: no kernel map");
    let offset = physical.0 % PAGE_SIZE;
    let range = map.enter_anywhere(
        (offset + size).next_multiple_of(PAGE_SIZE),
        Protection::READ | Protection::WRITE,
        Protection::all(),
    )?;
    arch::Pmap::enter_device(range.clone(), PhysAddr(physical.0 - offset));
    Ok(NonNull::new(ptr::with_exposed_provenance_mut(range.start + offset)).unwrap())
}
