// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Mach 4's calls' server routines, as MIG makes them from `mach4`'s
//! definitions; only those written so far.

use core::mem::size_of;
use core::ptr::{self, NonNull};

use super::mig::argument::{Argument, type_word};
use super::mig::reply::ReplyHeader;
use crate::error::Error;
use crate::ipc::MessageType;
use crate::ipc::kernel_message::KernelMessage;
use crate::ipc::kernel_message::header::Header;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::vm::proxy::Proxy;
use crate::vm::{Protection, VirtualAddress, user};

/// The routine serving a request of `id`, if one is written.
pub(super) fn routine(id: i32) -> Option<fn(&mut KernelMessage, &mut KernelMessage)> {
    match id {
        4010 => Some(memory_object_create_proxy),
        4011 => Some(vm_region_create_proxy),
        _ => None,
    }
}

/// `memory_object_create_proxy`'s request, as MIG lays it out for one
/// object.
#[repr(C)]
struct CreateProxyRequest {
    /// Its header.
    header: Header,
    /// The most the proxy may allow.
    max_protection: Argument<u32>,
    /// The object's type word.
    object_word: u64,
    /// The object's port.
    object: usize,
    /// The offset's type word.
    offset_word: u64,
    /// The offset in the object.
    offset: usize,
    /// The start's type word.
    start_word: u64,
    /// Where in the object the part starts.
    start: usize,
    /// The length's type word.
    len_word: u64,
    /// How many bytes the part takes.
    len: usize,
}

const _: () = assert!(size_of::<CreateProxyRequest>() == 112);

/// `vm_region_create_proxy`'s request.
#[repr(C)]
struct RegionProxyRequest {
    /// Its header.
    header: Header,
    /// The address in the region.
    address: Argument<usize>,
    /// The most the proxy may allow.
    max_protection: Argument<u32>,
    /// How many bytes from the region's start.
    len: Argument<usize>,
}

const _: () = assert!(size_of::<RegionProxyRequest>() == 80);

/// Both routines' reply: the code, then the proxy's send right.
#[repr(C)]
struct ProxyReply {
    /// Its header and code.
    head: ReplyHeader,
    /// The proxy's type word and port.
    proxy: Argument<usize>,
}

const _: () = assert!(size_of::<ProxyReply>() == 64);

/// Serves `memory_object_create_proxy`: a proxy for `start` through `len`
/// of `object` from `offset`, allowing at most `max_protection`, in
/// `task`'s space.
///
/// # Panics
///
/// On a request MIG refuses, or one for more than one object, from another
/// offset, or with arrays of other counts: not yet.
fn memory_object_create_proxy(request: &mut KernelMessage, reply: &mut KernelMessage) {
    // SAFETY: `CreateProxyRequest` is `repr(C)`, a `Header`, then integers
    // and a port's address only.
    let request = unsafe { request.layout::<CreateProxyRequest>() };
    assert!(
        usize::try_from(request.header.size) == Ok(size_of::<CreateProxyRequest>())
            && request.max_protection.word == type_word(MessageType::Integer32, 32, 1)
            && request.object_word == type_word(MessageType::PortSend, 64, 1)
            && request.offset_word == type_word(MessageType::Integer64, 64, 1)
            && request.start_word == type_word(MessageType::Integer64, 64, 1)
            && request.len_word == type_word(MessageType::Integer64, 64, 1),
        "memory_object_create_proxy: MIG_BAD_ARGUMENTS, not yet"
    );
    // SAFETY: copied in, the destination is a port's exposed address, and
    // the request holds a reference to it.
    let destination = unsafe { &*ptr::with_exposed_provenance::<Port>(request.header.remote_port) };
    if destination.to_task().is_none() {
        return failure(reply, Error::InvalidTask);
    }
    // SAFETY: copied in, an inline port's data is the exposed address of a
    // reference to it given up by `into_raw`, which the proxy takes.
    let Some(object) = NonNull::new(ptr::with_exposed_provenance_mut::<Port>(request.object))
        .map(|object| unsafe { Reference::from_raw(object) })
    else {
        return failure(reply, Error::InvalidName);
    };
    if request.offset != 0 {
        drop(object);
        return failure(reply, Error::InvalidArgument);
    }
    let Some(end) = request.start.checked_add(request.len) else {
        drop(object);
        return failure(reply, Error::InvalidArgument);
    };
    match Proxy::create(
        object,
        Protection::from_bits_retain(request.max_protection.data),
        request.start..end,
    ) {
        Ok(proxy) => success(reply, proxy),
        Err(error) => failure(reply, error),
    }
}

/// Serves `vm_region_create_proxy`: a proxy for `len` bytes of the region
/// `address` is in of `task`, allowing at most `max_protection`.
///
/// # Errors
///
/// As `user::region_create_proxy`.
///
/// # Panics
///
/// On a request MIG refuses: not yet.
fn vm_region_create_proxy(request: &mut KernelMessage, reply: &mut KernelMessage) {
    // SAFETY: `RegionProxyRequest` is `repr(C)`, a `Header`, then integers
    // only.
    let request = unsafe { request.layout::<RegionProxyRequest>() };
    assert!(
        usize::try_from(request.header.size) == Ok(size_of::<RegionProxyRequest>())
            && request.address.word == type_word(MessageType::Integer64, 64, 1)
            && request.max_protection.word == type_word(MessageType::Integer32, 32, 1)
            && request.len.word == type_word(MessageType::Integer64, 64, 1),
        "vm_region_create_proxy: MIG_BAD_ARGUMENTS, not yet"
    );
    // SAFETY: copied in, the destination is a port's exposed address, and
    // the request holds a reference to it.
    let destination = unsafe { &*ptr::with_exposed_provenance::<Port>(request.header.remote_port) };
    let Some(task) = destination.to_task() else {
        return failure(reply, Error::InvalidArgument);
    };
    let result = user::region_create_proxy(
        &task.map,
        VirtualAddress::from(request.address.data),
        Protection::from_bits_retain(request.max_protection.data),
        request.len.data,
    );
    match result {
        Ok(proxy) => success(reply, proxy),
        Err(error) => failure(reply, error),
    }
}

/// Answers the request with `error`.
fn failure(reply: &mut KernelMessage, error: Error) {
    // SAFETY: MIG's reply header, `repr(C)`, a `Header` first, integers only.
    unsafe { reply.layout_mut::<ReplyHeader>() }
        .return_code
        .data = u32::from(error);
}

/// Answers the request with a send right for `proxy`.
fn success(reply: &mut KernelMessage, proxy: Reference<Port>) {
    // SAFETY: `ProxyReply` is `repr(C)`, a `ReplyHeader`, then a port's
    // address only.
    let reply = unsafe { reply.layout_mut::<ProxyReply>() };
    reply.head.return_code.data = 0;
    reply.head.header.size = u32::try_from(size_of::<ProxyReply>()).unwrap();
    reply.head.header.bits = reply.head.header.bits.with_complex();
    reply.proxy.word = type_word(MessageType::PortSend, 64, 1);
    reply.proxy.data = Reference::into_raw(proxy).as_ptr().expose_provenance();
}
