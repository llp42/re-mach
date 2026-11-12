// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The host calls' server routines, as MIG makes them from `mach_host`'s
//! definitions; only those written so far.

use core::mem::offset_of;
use core::ptr;

use super::{INTEGER_32_TYPE, type_word};
use crate::ipc::MessageType;
use crate::ipc::kernel_message::{Header, KernelMessage};
use crate::ipc::port::Port;
use crate::{host, ipc_host, machine};

/// The subsystem's first id.
const BASE: i32 = 2600;

/// The routine serving a request of `id`, if one is written.
pub(super) fn routine(id: i32) -> Option<fn(&KernelMessage, &mut KernelMessage)> {
    match id.wrapping_sub(BASE) {
        35 => Some(host_reboot),
        38 => Some(host_info),
        46 => Some(host_get_kernel_version),
        _ => None,
    }
}

/// `host_reboot`'s request, as MIG lays it out.
#[repr(C)]
struct RebootRequest {
    /// The request's header.
    header: Header,
    /// The options' type word.
    options_type: u64,
    /// The options.
    options: i32,
    /// Padding to 8 bytes.
    options_pad: [u8; 4],
}

/// Serves `host_reboot`, as MIG's stub does: a request of 48 bytes, not
/// complex, its options one inline 32-bit integer, goes to
/// `machine::host_reboot` with the host its destination stands for.
///
/// # Panics
///
/// On any other request: not yet; as `machine::host_reboot`.
fn host_reboot(request: &KernelMessage, _reply: &mut KernelMessage) {
    // SAFETY: MIG's layout of the request, `repr(C)`, a `Header` first,
    // integers only.
    let request = unsafe { request.layout::<RebootRequest>() };
    assert!(
        request.header.size == 48
            && !request.header.bits.is_complex()
            && request.options_type == INTEGER_32_TYPE,
        "host_reboot: MIG_BAD_ARGUMENTS, not yet"
    );
    // SAFETY: copied in, the destination is a port's exposed address, and
    // the request holds a reference to it.
    let port = unsafe { &*ptr::with_exposed_provenance::<Port>(request.header.remote_port) };
    machine::host_reboot(ipc_host::privileged_host(port), request.options)
}

/// `host_info`'s request, as MIG lays it out.
#[repr(C)]
struct InfoRequest {
    /// The request's header.
    header: Header,
    /// The flavor's type word.
    flavor_type: u64,
    /// The flavor.
    flavor: i32,
    /// Padding to 8 bytes.
    flavor_pad: [u8; 4],
    /// The count's type word.
    count_type: u64,
    /// The count of integers asked for.
    count: u32,
    /// Padding to 8 bytes.
    count_pad: [u8; 4],
}

/// `host_info`'s reply, as MIG lays it out.
#[repr(C)]
struct InfoReply {
    /// The reply's header.
    header: Header,
    /// The return code's type word.
    return_code_type: u64,
    /// The routine's return code.
    return_code: u32,
    /// Padding to 8 bytes.
    return_code_pad: [u8; 4],
    /// The information's type word.
    info_type: u64,
    /// The information, its first integers used.
    info: [i32; 1024],
}

/// Serves `host_info`, as MIG's stub does: a request of 64 bytes, not
/// complex, its flavor and count one inline 32-bit integer each, gets up to
/// 1024 integers from `host::info` with the host its destination stands
/// for; the reply holds those used.
///
/// # Panics
///
/// On any other request: not yet; as `host::info`.
fn host_info(request: &KernelMessage, reply: &mut KernelMessage) {
    // SAFETY: MIG's layout of the request, `repr(C)`, a `Header` first,
    // integers only.
    let request = unsafe { request.layout::<InfoRequest>() };
    assert!(
        request.header.size == 64
            && !request.header.bits.is_complex()
            && request.flavor_type == INTEGER_32_TYPE
            && request.count_type == INTEGER_32_TYPE,
        "host_info: MIG_BAD_ARGUMENTS, not yet"
    );
    // SAFETY: MIG's layout of the reply, `repr(C)`, a `Header` first,
    // integers only.
    let reply = unsafe { reply.layout_mut::<InfoReply>() };
    let count = usize::try_from(request.count)
        .unwrap()
        .min(reply.info.len());
    // SAFETY: as in `host_reboot`.
    let port = unsafe { &*ptr::with_exposed_provenance::<Port>(request.header.remote_port) };
    let count = match host::info(
        ipc_host::host(port),
        request.flavor,
        &mut reply.info[..count],
    ) {
        Ok(count) => count,
        Err(error) => {
            reply.return_code = error.into();
            return;
        }
    };
    reply.return_code = 0;
    reply.info_type = type_word(MessageType::Integer32, 32, u32::try_from(count).unwrap());
    // The integers used, padded to 8 bytes.
    let size = offset_of!(InfoReply, info) + (size_of::<i32>() * count).next_multiple_of(8);
    reply.header.size = u32::try_from(size).unwrap();
}

/// `host_get_kernel_version`'s reply, as MIG lays it out.
#[repr(C)]
struct KernelVersionReply {
    /// The reply's header.
    header: Header,
    /// The return code's type word.
    return_code_type: u64,
    /// The routine's return code.
    return_code: u32,
    /// Padding to 8 bytes.
    return_code_pad: [u8; 4],
    /// The version's type word.
    kernel_version_type: u64,
    /// The version.
    kernel_version: [u8; 512],
}

/// Serves `host_get_kernel_version`, as MIG's stub does: a request of its
/// header alone, not complex, gets the kernel's version from
/// `host::kernel_version` with the host its destination stands for.
///
/// # Panics
///
/// On any other request: not yet.
fn host_get_kernel_version(request: &KernelMessage, reply: &mut KernelMessage) {
    let header = request.header();
    assert!(
        header.size == 32 && !header.bits.is_complex(),
        "host_get_kernel_version: MIG_BAD_ARGUMENTS, not yet"
    );
    // SAFETY: as in `host_reboot`.
    let port = unsafe { &*ptr::with_exposed_provenance::<Port>(header.remote_port) };
    // SAFETY: MIG's layout of the reply, `repr(C)`, a `Header` first,
    // integers only.
    let reply = unsafe { reply.layout_mut::<KernelVersionReply>() };
    reply.return_code = host::kernel_version(ipc_host::host(port), &mut reply.kernel_version)
        .map_or_else(u32::from, |()| 0);
    if reply.return_code != 0 {
        return;
    }
    reply.header.size = u32::try_from(size_of::<KernelVersionReply>()).unwrap();
    reply.kernel_version_type = type_word(MessageType::StringC, 8, 512);
}
