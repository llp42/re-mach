// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Serving the messages sent to ports the kernel receives on.

mod mach_host;

use crate::ipc::kmsg::{Header, Kmsg};

/// The size of a reply's buffer.
const REPLY_SIZE: usize = 8192;

/// MIG's type word for one inline 32-bit integer: name 2, 32 bits, one.
const INTEGER_32_TYPE: u64 = 0x0000_0001_2000_2002;

/// The size of MIG's reply header: the header, the return code's type word
/// and the return code, padded to 8 bytes.
const REPLY_HEADER_SIZE: u32 = 48;

/// Serves `request`, sent to a port the kernel receives on, as
/// `ipc_kobject_server` does: a reply with its header filled in, for the
/// routine its id names to finish; so far up to that routine.
///
/// # Panics
///
/// If no reply can be allocated, or no routine serves the request's id:
/// not yet; as the routine.
#[expect(
    clippy::needless_pass_by_value,
    reason = "ipc_kobject_server frees or destroys the request"
)]
pub fn server(request: Kmsg) -> ! {
    let mut reply = Kmsg::allocate(REPLY_SIZE)
        .unwrap_or_else(|_| panic!("ipc_kobject_server: dropping request, not yet"));
    let header = request.header();
    // To the request's reply port, with the right it carries.
    *reply.header_mut() = Header {
        bits: (header.bits >> 8) & 0xff,
        size: REPLY_HEADER_SIZE,
        remote_port: header.local_port,
        local_port: 0,
        sequence_number: 0,
        id: header.id.wrapping_add(100),
    };
    reply.body_mut()[..8].copy_from_slice(&INTEGER_32_TYPE.to_le_bytes());
    let routine = mach_host::routine(header.id)
        .unwrap_or_else(|| panic!("ipc_kobject_server: MIG_BAD_ID, not yet"));
    routine(&request, &mut reply)
}
