// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The host calls' server routines, as MIG makes them from `mach_host`'s
//! definitions; only `host_reboot` so far.

use core::ptr;

use super::INTEGER_32_TYPE;
use crate::ipc::kernel_message::{COMPLEX, KernelMessage};
use crate::ipc::port::Port;
use crate::{ipc_host, machine};

/// The subsystem's first id.
const BASE: i32 = 2600;

/// The routine serving a request of `id`, if one is written.
pub(super) fn routine(id: i32) -> Option<fn(&KernelMessage, &mut KernelMessage) -> !> {
    match id.wrapping_sub(BASE) {
        35 => Some(host_reboot),
        _ => None,
    }
}

/// Serves `host_reboot`, as MIG's stub does: a request of 48 bytes, not
/// complex, its options one inline 32-bit integer, goes to
/// `machine::host_reboot` with the host its destination stands for.
///
/// # Panics
///
/// On any other request: not yet; as `machine::host_reboot`.
fn host_reboot(request: &KernelMessage, _reply: &mut KernelMessage) -> ! {
    let header = request.header();
    let body = request.body();
    // The header, the options' type word, the options and 4 bytes of padding.
    assert!(
        header.size == 48
            && header.bits & COMPLEX == 0
            && u64::from_le_bytes(body[..8].try_into().unwrap()) == INTEGER_32_TYPE,
        "host_reboot: MIG_BAD_ARGUMENTS, not yet"
    );
    let options = i32::from_le_bytes(body[8..12].try_into().unwrap());
    // SAFETY: copied in, the destination is a port's exposed address, and
    // the request holds a reference to it.
    let port = unsafe { &*ptr::with_exposed_provenance::<Port>(header.remote_port) };
    machine::host_reboot(ipc_host::privileged_host(port), options)
}
