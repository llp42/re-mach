// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The x86 calls' server routines, as MIG makes them from `mach_i386`'s
//! definitions; only those written so far.

use core::ptr;
use core::sync::atomic::Ordering;

use super::fpu;
use crate::device;
use crate::error::Error;
use crate::host::Host;
use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::ipc_kobject::mig;
use crate::reference::Reference;

mig::subsystem! {
    base 3800;
    5 i386_io_perm_create(master: Port, from: u16, to: u16) -> (io_perm: Port);
    9 i386_get_xstate_size(host = Port::as_host) -> (size: usize);
}

/// Serves `i386_io_perm_create`: a port for the I/O ports `from` through
/// `to`, made through `master`, the device master port.
///
/// # Errors
///
/// `InvalidArgument` if `master` is not the device master port, or `from`
/// is above `to`; as `Port::allocate_kernel`.
fn i386_io_perm_create(master: &Port, from: u16, to: u16) -> Result<Reference<Port>, Error> {
    if !ptr::eq(master, device::MASTER_PORT.load(Ordering::Acquire)) || from > to {
        return Err(Error::InvalidArgument);
    }
    let port = Port::allocate_kernel()?;
    port.set_kernel_object(KernelObject::IoPerm { from, to });
    let port = Reference::from(port);
    Ok(Port::make_send(&port))
}

/// Serves `i386_get_xstate_size`: the bytes `i386_XFLOAT_STATE` takes, for
/// `host`.
///
/// # Errors
///
/// `InvalidArgument` for no host.
fn i386_get_xstate_size(host: Option<&&Host>) -> Result<usize, Error> {
    if host.is_none() {
        return Err(Error::InvalidArgument);
    }
    Ok(fpu::state::xstate_size())
}
