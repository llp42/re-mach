// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The x86 calls' server routines, as MIG makes them from `mach_i386`'s
//! definitions; only those written so far.

use super::fpu;
use crate::error::Error;
use crate::host::Host;
use crate::ipc::port::Port;
use crate::ipc_kobject::mig;

mig::subsystem! {
    base 3800;
    9 i386_get_xstate_size(host = Port::as_host) -> (size: usize);
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
    Ok(fpu::xstate_size())
}
