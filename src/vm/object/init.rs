// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use super::object::{CACHE, KERNEL_OBJECT, Object};
use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::slab::CacheBox;

/// Sets up memory objects, on the boot CPU.
pub fn bootstrap() {
    CACHE.register();
}

/// Gives the kernel object its name port, on the boot CPU.
///
/// # Panics
///
/// If no port can be allocated.
pub fn init() {
    let port = Port::allocate_kernel()
        .unwrap_or_else(|error| panic!("vm_object_init: ipc_port_alloc_kernel failed: {error:?}"));
    port.set_kernel_object(KernelObject::PagerName(NonNull::from(&KERNEL_OBJECT)));
    // The slot keeps its reference.
    KERNEL_OBJECT.pager_name.store(
        Reference::into_raw(Reference::from(port)).as_ptr(),
        Ordering::Release,
    );
}

/// A new internal, temporary object of `size` bytes, with one reference
/// and a name port.
///
/// # Panics
///
/// If the object or its port can't be allocated.
pub fn allocate(size: usize) -> Reference<Object> {
    let object = CACHE
        .allocate(Object::new(size))
        .unwrap_or_else(|error| panic!("vm_object_allocate: {error:?}"));
    // SAFETY: a new object's box holds its first reference, given up here.
    let object = unsafe { Reference::from_raw(CacheBox::into_raw(object)) };
    let port =
        Port::allocate_kernel().unwrap_or_else(|error| panic!("vm_object_allocate: {error:?}"));
    port.set_kernel_object(KernelObject::PagerName(Reference::as_ptr(&object)));
    // The slot keeps its reference.
    object.pager_name.store(
        Reference::into_raw(Reference::from(port)).as_ptr(),
        Ordering::Release,
    );
    object
}
