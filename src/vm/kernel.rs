// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Kernel memory: the kernel's map.

use core::ptr::{self, NonNull};

use super::map::Map;
use super::object::KERNEL_OBJECT;
use super::page::{self, Page};
use super::phys_map::MachinePhysMap as _;
use super::{Inheritance, Protection};
use core::ops::Range;

use crate::arch::{
    self, KERNEL_STACK_SIZE, MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE, PhysAddr,
};
use crate::error::Error;
use crate::sync::OnceLock;

/// The kernel's map, once `init` sets it up.
pub static KERNEL_MAP: OnceLock<Map> = OnceLock::new();

/// The kernel's map.
///
/// # Panics
///
/// If it is not set up.
#[track_caller]
fn kernel_map() -> &'static Map {
    KERNEL_MAP.get().expect("vm_kernel: no kernel map")
}

/// Sets up the kernel's map, with the boot mappings in use, on the boot CPU.
///
/// # Panics
///
/// If the boot mappings can't be entered, or on a second call.
pub fn init() {
    // A range can't end past the last page, so the map stops before it.
    let bounds = MIN_KERNEL_ADDRESS..MAX_KERNEL_ADDRESS - PAGE_SIZE + 1;
    let map = Map::new(arch::PhysMap::kernel(), bounds);
    if let Err(error) = map.enter(
        arch::PhysMap::boot_space(),
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
    let map = kernel_map();
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
    let map = kernel_map();
    let offset = physical.0 % PAGE_SIZE;
    let range = map.enter_anywhere(
        (offset + size).next_multiple_of(PAGE_SIZE),
        PAGE_SIZE,
        Protection::READ | Protection::WRITE,
        Protection::all(),
        None,
    )?;
    arch::PhysMap::enter_device(range.clone(), PhysAddr(physical.0 - offset));
    Ok(NonNull::new(ptr::with_exposed_provenance_mut(range.start + offset)).unwrap())
}

/// Takes `size` bytes of wired kernel memory, rounded up to pages, from the
/// kernel object, zeroed; returns where it is mapped.
///
/// # Errors
///
/// `NoSpace` if the kernel map has no room for it, and `ResourceShortage`
/// if no entry or page can be allocated, which leaves what was taken.
///
/// # Panics
///
/// If the kernel map is not set up.
pub fn allocate_wired(size: usize) -> Result<NonNull<u8>, Error> {
    let map = kernel_map();
    let range = map.enter_anywhere(
        size.next_multiple_of(PAGE_SIZE),
        PAGE_SIZE,
        Protection::READ | Protection::WRITE,
        Protection::all(),
        Some(&KERNEL_OBJECT),
    )?;
    fill(range.clone())?;
    Ok(NonNull::new(ptr::with_exposed_provenance_mut(range.start)).unwrap())
}

/// Takes `size` bytes of wired kernel memory, a power of two rounded up to
/// pages, aligned to their size, from the kernel object, zeroed; returns
/// where it is mapped.
///
/// # Errors
///
/// As `allocate_wired`.
///
/// # Panics
///
/// If `size` is not a power of two, or the kernel map is not set up.
pub fn allocate_aligned(size: usize) -> Result<NonNull<u8>, Error> {
    assert!(size.is_power_of_two(), "kmem_alloc_aligned");
    let map = kernel_map();
    let size = size.next_multiple_of(PAGE_SIZE);
    let range = map.enter_anywhere(
        size,
        size,
        Protection::READ | Protection::WRITE,
        Protection::all(),
        Some(&KERNEL_OBJECT),
    )?;
    fill(range.clone())?;
    Ok(NonNull::new(ptr::with_exposed_provenance_mut(range.start)).unwrap())
}

/// Where `page`, a page of the kernel object, is mapped.
pub fn address_of(page: &Page) -> usize {
    MIN_KERNEL_ADDRESS + page.offset()
}

/// The page kernel `address` is mapped to; `None` if none, or one outside
/// the page array.
pub fn page_of(address: usize) -> Option<&'static Page> {
    arch::PhysMap::extract_kernel(address).and_then(page::lookup)
}

/// Maps a zeroed, wired page of the kernel object at each page of `range`,
/// at the offset of its address in the kernel map.
fn fill(range: Range<usize>) -> Result<(), Error> {
    for address in range.step_by(PAGE_SIZE) {
        let offset = address - MIN_KERNEL_ADDRESS;
        // The kernel object's colour is 0.
        let page = page::allocate_zeroed(offset / PAGE_SIZE).ok_or(Error::ResourceShortage)?;
        let mut state = KERNEL_OBJECT.lock();
        state.insert(&KERNEL_OBJECT, page, offset);
        page.wire();
        drop(state);
        arch::PhysMap::enter_kernel(address, page.address());
    }
    Ok(())
}

/// A thread's kernel stack, which it keeps for life.
pub struct KernelStack {
    /// Past its last byte, where pushing starts.
    top: usize,
}

impl KernelStack {
    /// Past its last byte, where pushing starts.
    pub const fn top(&self) -> usize {
        self.top
    }
}

/// Takes a kernel stack: `KERNEL_STACK_SIZE` bytes of wired, zeroed memory
/// of the kernel object, above a page left unmapped, so running off its
/// end faults.
///
/// # Errors
///
/// As `allocate_wired`.
///
/// # Panics
///
/// If the kernel map is not set up.
pub fn allocate_stack() -> Result<KernelStack, Error> {
    let map = kernel_map();
    let range = map.enter_anywhere(
        PAGE_SIZE + KERNEL_STACK_SIZE,
        PAGE_SIZE,
        Protection::READ | Protection::WRITE,
        Protection::all(),
        Some(&KERNEL_OBJECT),
    )?;
    fill(range.start + PAGE_SIZE..range.end)?;
    Ok(KernelStack { top: range.end })
}
