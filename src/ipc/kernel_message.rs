// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Kernel messages: messages in kernel buffers.

use core::mem::offset_of;
use core::ptr::NonNull;
use core::slice;

use crate::arch::{self, Machine as _, PAGE_SIZE};
use crate::error::Error;
use crate::kalloc;

/// A message's header, in Mach's layout. Its ports are names as the user
/// sent them, then, once copied in, ports' exposed addresses.
#[repr(C)]
pub struct Header {
    /// Its ports' right types, and whether it is complex.
    pub bits: u32,
    /// Its size, in bytes.
    pub size: u32,
    /// Its destination.
    pub remote_port: usize,
    /// Its reply port.
    pub local_port: usize,
    /// Its sequence number on the port it is received from.
    pub sequence_number: u32,
    /// What it asks for.
    pub id: i32,
}

/// The header bit of a message whose body holds rights or out-of-line
/// memory.
pub const COMPLEX: u32 = 1 << 31;

/// A kernel message: its buffer, from kalloc.
pub struct KernelMessage(NonNull<Buffer>);

/// A kernel message's buffer: its size, then the message, header first.
#[repr(C)]
struct Buffer {
    /// Its size, in bytes.
    size: usize,
    /// The message's header.
    header: Header,
}

/// The bytes a buffer holds before its message.
const OVERHEAD: usize = offset_of!(Buffer, header);

/// How much a message may grow in the kernel: a port name's 4 bytes become
/// a port's 8.
const EXPAND_FACTOR: usize = 2;

impl KernelMessage {
    /// A new one, in a zeroed kalloc buffer of `size` bytes.
    ///
    /// # Errors
    ///
    /// As `kalloc::allocate`.
    ///
    /// # Panics
    ///
    /// If `size` can't hold a header; as `kalloc::allocate`.
    pub fn allocate(size: usize) -> Result<Self, Error> {
        assert!(
            size >= size_of::<Buffer>(),
            "ikm_alloc: no room for a header"
        );
        let buffer = kalloc::allocate(size)?.cast::<Buffer>();
        // SAFETY: a buffer of at least `size` bytes, ours alone, aligned for
        // a `Buffer`.
        unsafe {
            buffer.cast::<u8>().write_bytes(0, size);
            (&raw mut (*buffer.as_ptr()).size).write(size);
        }
        Ok(Self(buffer))
    }

    /// Its header.
    pub fn header(&self) -> &Header {
        // SAFETY: the buffer holds a header, and is ours alone.
        unsafe { &(*self.0.as_ptr()).header }
    }

    /// Its header, to change.
    pub fn header_mut(&mut self) -> &mut Header {
        // SAFETY: as in `header`.
        unsafe { &mut (*self.0.as_ptr()).header }
    }

    /// Its body: the bytes past its header, to the buffer's end.
    pub fn body(&self) -> &[u8] {
        let buffer = self.0.as_ptr();
        // SAFETY: the buffer's bytes past the header, ours alone.
        unsafe {
            slice::from_raw_parts(
                buffer.cast::<u8>().add(size_of::<Buffer>()),
                (*buffer).size - size_of::<Buffer>(),
            )
        }
    }

    /// Its body, to change.
    pub fn body_mut(&mut self) -> &mut [u8] {
        &mut self.message_mut()[size_of::<Header>()..]
    }

    /// Its message, header then body, to the buffer's end.
    #[expect(
        clippy::needless_pass_by_ref_mut,
        reason = "the bytes are the buffer's, which it owns"
    )]
    fn message_mut(&mut self) -> &mut [u8] {
        let buffer = self.0.as_ptr();
        // SAFETY: the buffer's bytes from the header on, ours alone.
        unsafe {
            slice::from_raw_parts_mut(buffer.cast::<u8>().add(OVERHEAD), (*buffer).size - OVERHEAD)
        }
    }
}

/// Copies the user's message at `address`, `size` bytes, into a new kernel
/// message, as `ipc_kmsg_get` does; its ports keep their names' 32 bits.
///
/// # Panics
///
/// If `size` is below a header or not a multiple of 8, if the message
/// might outgrow a page, if no buffer can be allocated, or if the message
/// can't be copied in: not yet.
pub fn get(address: usize, size: u32) -> KernelMessage {
    let length = usize::try_from(size).unwrap();
    assert!(
        length >= size_of::<Header>() && length.is_multiple_of(8),
        "ipc_kmsg_get: MACH_SEND_MSG_TOO_SMALL, not yet"
    );
    assert!(
        length * EXPAND_FACTOR <= PAGE_SIZE - OVERHEAD,
        "ipc_kmsg_get: a buffer beyond a page, not yet"
    );
    let mut message = KernelMessage::allocate(PAGE_SIZE)
        .unwrap_or_else(|_| panic!("ipc_kmsg_get: MACH_SEND_NO_BUFFER, not yet"));
    assert!(
        arch::Current::copy_in(address, &mut message.message_mut()[..length]).is_ok(),
        "ipc_kmsg_get: MACH_SEND_INVALID_DATA, not yet"
    );
    let header = message.header_mut();
    header.size = size;
    header.remote_port &= 0xffff_ffff;
    header.local_port &= 0xffff_ffff;
    message
}
