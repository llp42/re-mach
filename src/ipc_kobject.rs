// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Serving the messages sent to ports the kernel receives on.

mod mach_host;

use core::ptr;
use core::sync::atomic::Ordering;

use crate::ipc::MessageType;
use crate::ipc::kernel_message::{Header, KernelMessage, MessageBits};
use crate::ipc::port::Port;
use crate::task;

/// The size of a reply's buffer.
const REPLY_SIZE: usize = 8192;

/// MIG's type word for one inline 32-bit integer.
const INTEGER_32_TYPE: u64 = type_word(MessageType::Integer32, 32, 1);

/// MIG's reply header, `mig_reply_header_t`: the header, the return code's
/// type word, then the return code.
#[repr(C)]
struct ReplyHeader {
    /// The reply's header.
    header: Header,
    /// The return code's type word.
    return_code_type: u64,
    /// The routine's return code.
    return_code: u32,
}

/// MIG's type word, Mach's `mach_msg_type_t`, for `number` inline items of
/// type `name`, `size` bits each.
const fn type_word(name: MessageType, size: u16, number: u32) -> u64 {
    let [size_low, size_high] = size.to_le_bytes();
    let [number_0, number_1, number_2, number_3] = number.to_le_bytes();
    // The name, the size, then the inline bit, bit 29; the number above.
    u64::from_le_bytes([
        name.number(),
        size_low,
        size_high,
        1 << 5,
        number_0,
        number_1,
        number_2,
        number_3,
    ])
}

/// Serves `request`, sent to a port the kernel receives on, as
/// `ipc_kobject_server` does: a reply with its header filled in, for the
/// routine its id names to finish; so far through that routine, counted in
/// the kernel task's messages, and the release of the request's
/// destination right.
///
/// # Panics
///
/// If no reply can be allocated, or no routine serves the request's id:
/// not yet; as the routine. If the destination right is neither a send nor
/// a send-once right; for a send-once right: not yet; as
/// `Port::release_send`. Then, at the request's free: not yet.
pub fn server(mut request: KernelMessage) -> ! {
    let mut reply = KernelMessage::allocate(REPLY_SIZE)
        .unwrap_or_else(|_| panic!("ipc_kobject_server: dropping request, not yet"));
    let header = request.header();
    // SAFETY: MIG's reply header, `repr(C)`, a `Header` first, integers only.
    let reply_header = unsafe { reply.layout_mut::<ReplyHeader>() };
    // To the request's reply port, with the right it carries.
    reply_header.header = Header {
        bits: MessageBits::new(header.bits.local(), None),
        size: u32::try_from(size_of::<ReplyHeader>()).unwrap(),
        remote_port: header.local_port,
        local_port: 0,
        sequence_number: 0,
        id: header.id.wrapping_add(100),
    };
    reply_header.return_code_type = INTEGER_32_TYPE;
    let routine = mach_host::routine(header.id)
        .unwrap_or_else(|| panic!("ipc_kobject_server: MIG_BAD_ID, not yet"));
    routine(&request, &mut reply);
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
    // SAFETY: copied in, the destination is a port's exposed address, and
    // the request holds a reference to it.
    let destination = unsafe { &*ptr::with_exposed_provenance::<Port>(header.remote_port) };
    match header.bits.remote() {
        Some(MessageType::PortSend) => destination.release_send(),
        Some(MessageType::PortSendOnce) => {
            panic!("ipc_kobject_server: ipc_port_release_sonce, not yet")
        }
        _ => panic!("ipc_object_destroy: strange destination rights"),
    }
    // So that destroying the request does not release it again.
    header.remote_port = 0;
    panic!("ipc_kobject_server: ikm_cache_free, not yet")
}
