// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The ports of the host, processor sets and processors.

use core::sync::atomic::{AtomicPtr, Ordering};

use super::host::HOST;
use super::slab::CacheBox;
use crate::ipc::port::{self, KernelObject, Port};

/// Gives the host its ports, on the boot CPU.
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
