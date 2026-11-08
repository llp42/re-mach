// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The ports of the host, processor sets and processors.

use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use crate::host::{HOST, Host};
use crate::ipc::port::{self, KernelObject, Port};
use crate::processor::{BOOT_PROCESSOR, DEFAULT_SET, Processor, ProcessorSet};
use crate::slab::CacheBox;

/// Gives the host, the default set and the boot processor their ports, on
/// the boot CPU.
///
/// # Panics
///
/// If a port can't be allocated, or before the boot processor is recorded.
pub fn init() {
    HOST.port.store(
        allocate_port(Some(KernelObject::Host(&HOST))).as_ptr(),
        Ordering::Release,
    );
    HOST.privileged_port.store(
        allocate_port(Some(KernelObject::HostPrivileged(&HOST))).as_ptr(),
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
        set.active.then_some(KernelObject::ProcessorSet(object)),
    ));
    set.set_name_port(allocate_port(
        set.active.then_some(KernelObject::ProcessorSetName(object)),
    ));
    if set.active {
        set.reference();
        set.reference();
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

/// A new port the kernel receives on, standing for `object`.
///
/// # Panics
///
/// If the port can't be allocated.
fn allocate_port(object: Option<KernelObject>) -> NonNull<Port> {
    let port = port::allocate_kernel()
        .unwrap_or_else(|error| panic!("ipc_host_init: ipc_port_alloc_kernel failed: {error:?}"));
    if let Some(object) = object {
        port.set_kernel_object(object);
    }
    CacheBox::into_raw(port)
}
