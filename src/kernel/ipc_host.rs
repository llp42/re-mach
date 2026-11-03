// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The ports of the host, processor sets and processors.

use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering};

use super::host::HOST;
use super::processor::{DEFAULT_SET, ProcessorSet};
use super::slab::CacheBox;
use crate::ipc::port::{self, KernelObject, Port};

/// Gives the host and the default set their ports, on the boot CPU.
///
/// # Panics
///
/// If a port can't be allocated.
pub fn init() {
    store_port(&HOST.port, Some(KernelObject::Host(&HOST)));
    store_port(
        &HOST.privileged_port,
        Some(KernelObject::HostPrivileged(&HOST)),
    );
    init_set(&DEFAULT_SET);
}

/// Gives `set` its ports, standing for it while it is active; no one else
/// reaches it yet, so no lock.
///
/// # Panics
///
/// If a port can't be allocated.
fn init_set(set: &'static ProcessorSet) {
    let object = NonNull::from(set);
    store_port(
        &set.port,
        set.active.then_some(KernelObject::ProcessorSet(object)),
    );
    store_port(
        &set.name_port,
        set.active.then_some(KernelObject::ProcessorSetName(object)),
    );
    if set.active {
        set.references.fetch_add(2, Ordering::Relaxed);
    }
}

/// Stores in `slot` a new port the kernel receives on, standing for
/// `object`.
///
/// # Panics
///
/// If the port can't be allocated.
fn store_port(slot: &AtomicPtr<Port>, object: Option<KernelObject>) {
    let mut port = port::allocate_kernel()
        .unwrap_or_else(|error| panic!("ipc_host_init: ipc_port_alloc_kernel failed: {error:?}"));
    if let Some(object) = object {
        port.set_kernel_object(object);
    }
    slot.store(CacheBox::into_raw(port).as_ptr(), Ordering::Release);
}
