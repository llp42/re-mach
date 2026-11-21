// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The ports of the host, processor sets and processors.

use core::mem::ManuallyDrop;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use crate::host::{HOST, Host};
use crate::ipc::port::{KernelObject, Port};
use crate::processor::{BOOT_PROCESSOR, DEFAULT_SET, Processor, ProcessorSet};
use crate::reference::Reference;

/// Gives the host, the default set and the boot processor their ports, on
/// the boot CPU.
///
/// # Panics
///
/// If a port can't be allocated, or before the boot processor is recorded.
pub fn init() {
    // Each slot keeps its port's reference.
    HOST.port.store(
        Reference::into_raw(allocate_port(Some(KernelObject::Host(&HOST)))).as_ptr(),
        Ordering::Release,
    );
    HOST.privileged_port.store(
        Reference::into_raw(allocate_port(Some(KernelObject::HostPrivileged(&HOST)))).as_ptr(),
        Ordering::Release,
    );
    init_set(&DEFAULT_SET);
    init_processor(
        BOOT_PROCESSOR
            .get()
            .expect("ipc_host_init: no boot processor"),
    );
}

/// Gives `processor` its ports, standing for it.
///
/// # Panics
///
/// If a port can't be allocated.
pub fn init_processor(processor: &'static Processor) {
    processor.set_port(allocate_port(Some(KernelObject::Processor(processor))));
    processor.set_name_port(allocate_port(Some(KernelObject::ProcessorName(processor))));
}

/// Gives `set` its ports, standing for it while it is active; no one else
/// reaches it yet, so no lock.
///
/// # Panics
///
/// If a port can't be allocated.
pub fn init_set(set: &'static ProcessorSet) {
    let object = NonNull::from(set);
    set.set_port(allocate_port(
        set.is_active()
            .then_some(KernelObject::ProcessorSet(object)),
    ));
    set.set_name_port(allocate_port(
        set.is_active()
            .then_some(KernelObject::ProcessorSetName(object)),
    ));
    if set.is_active() {
        // SAFETY: the caller's reference keeps it alive, which this handle
        // borrows and never drops.
        let set = ManuallyDrop::new(unsafe { Reference::from_raw(object) });
        // Its ports', which their kernel objects keep.
        Reference::into_raw(Reference::clone(&set));
        Reference::into_raw(Reference::clone(&set));
    }
}

/// The host `port` stands for while alive, if it is one of a host's ports,
/// as `convert_port_to_host` finds it.
pub fn host(port: &Port) -> Option<&'static Host> {
    match port.kernel_object() {
        Some(KernelObject::Host(host) | KernelObject::HostPrivileged(host)) => Some(host),
        _ => None,
    }
}

/// The host `port` stands for while alive, if it is a host's privileged
/// port, as `convert_port_to_host_priv` finds it.
pub fn privileged_host(port: &Port) -> Option<&'static Host> {
    match port.kernel_object() {
        Some(KernelObject::HostPrivileged(host)) => Some(host),
        _ => None,
    }
}

/// The set `port` stands for, if it is a set's port, not its name port,
/// with a reference to it taken under the port's lock, as
/// `convert_port_to_pset` gives it.
pub fn set_of(port: &Port) -> Option<Reference<ProcessorSet>> {
    port.with_kernel_object(|object| match object {
        Some(KernelObject::ProcessorSet(set)) => {
            // SAFETY: a set's ports keep a reference to it while they stand
            // for it, which this handle borrows and never drops.
            let set = ManuallyDrop::new(unsafe { Reference::from_raw(*set) });
            Some(Reference::clone(&set))
        }
        _ => None,
    })
}

/// The set `port` stands for, if it is a set's port or name port, with a
/// reference to it taken under the port's lock, as
/// `convert_port_to_pset_name` gives it.
pub fn set_name_of(port: &Port) -> Option<Reference<ProcessorSet>> {
    port.with_kernel_object(|object| match object {
        Some(KernelObject::ProcessorSet(set) | KernelObject::ProcessorSetName(set)) => {
            // SAFETY: a set's ports keep a reference to it while they stand
            // for it, which this handle borrows and never drops.
            let set = ManuallyDrop::new(unsafe { Reference::from_raw(*set) });
            Some(Reference::clone(&set))
        }
        _ => None,
    })
}

/// A new port the kernel receives on, standing for `object`; gives its
/// reference.
///
/// # Panics
///
/// If the port can't be allocated.
fn allocate_port(object: Option<KernelObject>) -> Reference<Port> {
    let port = Port::allocate_kernel()
        .unwrap_or_else(|error| panic!("ipc_host_init: ipc_port_alloc_kernel failed: {error:?}"));
    if let Some(object) = object {
        port.set_kernel_object(object);
    }
    Reference::from(port)
}
