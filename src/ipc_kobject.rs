// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Serving the messages sent to ports the kernel receives on.

mod device;
mod gnumach;
mod mach;
mod mach_host;
mod mach_port;
pub mod mig;

use core::ptr::{self, NonNull};
use core::sync::atomic::Ordering;

use crate::arch::{self, Machine as _};
use crate::ipc::MessageType;
use crate::ipc::kernel_message::{Header, KernelMessage, MessageBits};
use crate::ipc::port::Port;
use crate::reference::Reference;
use crate::task;

/// The size of a reply's buffer.
const REPLY_SIZE: usize = 8192;

/// Serves `request`, sent to a port the kernel receives on, as
/// `ipc_kobject_server` does: a reply with its header filled in, for the
/// routine its id names to finish, counted in the kernel task's messages;
/// then the request's destination right is released and its buffer saved
/// or freed; if the routine failed, the request is destroyed instead.
/// Gives the reply, or `None` if the request has no reply port, the reply
/// destroyed.
///
/// # Panics
///
/// If no reply can be allocated, or no routine serves the request's id:
/// not yet; as the routine. If the destination right is neither a send nor
/// a send-once right; for a send-once right: not yet; as
/// `Port::release_send`. If the reply port is dead, or null for a complex
/// reply: not yet.
pub fn server(mut request: KernelMessage) -> Option<KernelMessage> {
    let mut reply = KernelMessage::allocate(REPLY_SIZE)
        .unwrap_or_else(|_| panic!("ipc_kobject_server: dropping request, not yet"));
    let header = request.header();
    // SAFETY: MIG's reply header, `repr(C)`, a `Header` first, integers only.
    let reply_header = unsafe { reply.layout_mut::<mig::ReplyHeader>() };
    // To the request's reply port, with the right it carries.
    reply_header.header = Header {
        bits: MessageBits::new(header.bits.local(), None),
        size: u32::try_from(size_of::<mig::ReplyHeader>()).unwrap(),
        remote_port: header.local_port,
        local_port: 0,
        sequence_number: 0,
        id: header.id.wrapping_add(100),
    };
    reply_header.return_code.word = <i32 as mig::Inline>::WORD;
    let routine = mach::routine(header.id)
        .or_else(|| mach_port::routine(header.id))
        .or_else(|| mach_host::routine(header.id))
        .or_else(|| device::routine(header.id))
        .or_else(|| gnumach::routine(header.id))
        .or_else(|| arch::Current::server_routine(header.id))
        .unwrap_or_else(|| panic!("ipc_kobject_server: MIG_BAD_ID, not yet"));
    routine(&mut request, &mut reply);
    let kernel_task = task::KERNEL_TASK
        .get()
        .expect("ipc_kobject_server: no kernel task");
    kernel_task
        .messages_received
        .fetch_add(1, Ordering::Relaxed);
    kernel_task.messages_sent.fetch_add(1, Ordering::Relaxed);
    // The destination's right is released: a send-once notification would
    // come back here.
    let header = request.header_mut();
    // SAFETY: copied in, the destination is the exposed address of a
    // reference to a port given up by `into_raw`, which the handle takes, as
    // the slot is cleared below.
    let destination = unsafe {
        Reference::from_raw(
            NonNull::new(ptr::with_exposed_provenance_mut(header.remote_port)).unwrap(),
        )
    };
    match header.bits.remote() {
        Some(MessageType::PortSend) => Port::release_send(destination),
        Some(MessageType::PortSendOnce) => {
            panic!("ipc_kobject_server: ipc_port_release_sonce, not yet")
        }
        _ => panic!("ipc_object_destroy: strange destination rights"),
    }
    // So that destroying the request does not release it again.
    header.remote_port = 0;
    // SAFETY: MIG's reply header, as for its header above.
    let return_code = unsafe { reply.layout::<mig::ReplyHeader>() }
        .return_code
        .data;
    // No routine gives MIG_NO_REPLY yet: `Error` has no such code.
    if return_code == 0 {
        // As ipc_kmsg_put, without the copy-out.
        request.cache_or_free();
    } else {
        // Freed, as ipc_kmsg_destroy; its reply port's right stays with the
        // reply.
        // The stub's handles released the body's rights already, before the
        // destination's (GNU Mach: the destination's first).
        request.header_mut().local_port = 0;
        drop(request);
    }
    // Queued only to a valid reply port, else destroyed, as
    // ipc_kmsg_destroy: a simple reply with no ports is only freed.
    match reply.header().remote_port {
        0 => {
            assert!(
                !reply.header().bits.is_complex(),
                "ipc_kmsg_clean: ipc_kmsg_clean_body, not yet"
            );
            drop(reply);
            None
        }
        usize::MAX => panic!("ipc_kobject_server: a dead reply port, not yet"),
        _ => Some(reply),
    }
}
