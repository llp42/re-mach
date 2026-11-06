// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Devices; so far only the master device port.

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::ipc::port::{self, Port};
use crate::slab::CacheBox;

/// The master device port, which devices are opened through; once
/// `create_service` makes it.
pub static MASTER_PORT: AtomicPtr<Port> = AtomicPtr::new(ptr::null_mut());

/// Creates the device service, in the kernel task: so far only the master
/// device port, a port the kernel receives on that stands for nothing.
///
/// # Panics
///
/// If the port can't be allocated.
pub fn create_service() {
    let port = port::allocate_kernel()
        .unwrap_or_else(|error| panic!("can't allocate master device port: {error:?}"));
    MASTER_PORT.store(CacheBox::into_raw(port).as_ptr(), Ordering::Release);
}
