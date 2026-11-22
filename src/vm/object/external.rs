// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::ManuallyDrop;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use intrusive_collections::UnsafeRef;

use super::init::allocate;
use super::object::Object;
use super::state::{CopyStrategy, Flags, Kind};
use crate::device::pager;
use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::vm::STATISTICS;

/// How many objects pagers outside the kernel's default one manage, as
/// `vm_statistics` reports them.
pub(super) static EXTERNAL_COUNT: AtomicU32 = AtomicU32::new(0);

/// The object of the memory object `pager`, of `size` bytes, made and set
/// up with its pager, as `vm_object_enter` gives it for an object outside
/// the kernel, with a reference for the caller; none if `pager` stands for
/// something else.
///
/// # Panics
///
/// If `pager` has an object, another call made one meanwhile, or `pager`
/// is a user's: not yet. If no request port can be allocated. As
/// `allocate` and `pager::init_pager`.
pub fn enter(pager: &Reference<Port>, size: usize) -> Option<Reference<Object>> {
    // A device pager's record, or none for a user's pager.
    let record = match pager.kernel_object() {
        Some(KernelObject::DevicePager(record)) => Some(record),
        None => None,
        Some(KernelObject::Pager(_)) => panic!("vm_object_enter: an existing object, not yet"),
        Some(_) => {
            STATISTICS.lookups.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    // Made first; setting it re-checks the port.
    let object = allocate(size);
    assert!(
        matches!(
            pager.replace_kernel_object(Some(KernelObject::Pager(Reference::as_ptr(&object)))),
            Some(KernelObject::DevicePager(_)) | None
        ),
        "vm_object_enter: an object made meanwhile, not yet"
    );
    STATISTICS.lookups.fetch_add(1, Ordering::Relaxed);
    let pager = Port::copy_send(pager);
    let request = Reference::from(
        Port::allocate_kernel()
            .unwrap_or_else(|error| panic!("vm_object_enter: pager request alloc: {error:?}")),
    );
    request.set_kernel_object(KernelObject::PagingRequest(Reference::as_ptr(&object)));
    let mut state = object.lock();
    state.flags.insert(Flags::PAGER_CREATED);
    state.pager = Some(pager);
    state.pager_request = Some(request);
    // External, not temporary, and not ready until its pager says so.
    state.flags.remove(Flags::INTERNAL | Flags::TEMPORARY);
    // SAFETY: a pager's record stays allocated while its device lists it,
    // as long as its object lives.
    state.kind = record.map(|record| Kind::Device(unsafe { UnsafeRef::from_raw(record.as_ptr()) }));
    drop(state);
    EXTERNAL_COUNT.fetch_add(1, Ordering::Relaxed);
    // memory_object_init: its rights, made from the request and name ports,
    // then the kernel's own pager called directly.
    let request_send = Port::make_send(object.lock().pager_request.as_ref().unwrap());
    // SAFETY: the object's slot holds a reference to its name port, which
    // this handle borrows and never drops.
    let name = ManuallyDrop::new(unsafe {
        Reference::from_raw(NonNull::new(object.pager_name.load(Ordering::Acquire)).unwrap())
    });
    let name_send = Port::make_send(&name);
    let record = record.unwrap_or_else(|| panic!("memory_object_init: a user pager, not yet"));
    // SAFETY: as for its kind above.
    pager::init_pager(unsafe { record.as_ref() }, request_send, name_send);
    object.lock().flags.insert(Flags::PAGER_INITIALIZED);
    // Nothing waits for it to be initialized yet, so none is woken.
    Some(object)
}

/// Readies the object whose request port `request` is, as its pager's
/// `memory_object_ready` asks, with `memory_object_set_attributes_common`:
/// not to persist, copied as `copy_strategy` says.
///
/// # Panics
///
/// If `request` is no object's request port: not yet. As `lookup` and, at
/// the object's last reference, `terminate`.
pub fn ready(request: &Port, copy_strategy: CopyStrategy) {
    let object = request
        .to_vm_object()
        .unwrap_or_else(|| panic!("memory_object_ready: KERN_INVALID_ARGUMENT, not yet"));
    let mut state = object.lock();
    // Nothing waits for it to be ready yet, so none is woken.
    state.flags.remove(Flags::CAN_PERSIST);
    state.flags.insert(Flags::PAGER_READY);
    state.copy_strategy = copy_strategy;
    drop(state);
    // vm_object_deallocate.
    drop(object);
}
