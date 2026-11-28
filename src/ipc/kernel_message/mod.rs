// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Kernel messages: messages in kernel buffers.

mod copy_in;
mod copy_out;
pub mod header;

use core::mem::{ManuallyDrop, offset_of};
use core::ptr::{self, NonNull};
use core::slice;

use tailq::{Link, TailQueue};

use header::Header;

use super::MessageType;
use super::port::port::Port;
use crate::arch::{self, Machine as _, PAGE_SIZE};
use crate::error::Error;
use crate::kalloc;
use crate::reference::Reference;
use crate::sync::CpuLocal;

/// A kernel message: its buffer, from kalloc.
pub struct KernelMessage(NonNull<Buffer>);

/// A kernel message's buffer: its place in a port's queue, its size, then
/// the message, header first.
#[repr(C)]
pub(super) struct Buffer {
    /// Its place in the queue of the port it waits on.
    link: Link,
    /// Its size, in bytes.
    size: usize,
    /// The message's header.
    header: Header,
}

tailq::adapter!(pub(super) QueueAdapter = Buffer { link });

impl Buffer {
    /// Its message's header.
    pub(super) const fn header(&self) -> &Header {
        &self.header
    }
}

/// The bytes a buffer holds before its message.
const OVERHEAD: usize = offset_of!(Buffer, header);

/// How much a message may grow in the kernel: a port name's 4 bytes become
/// a port's 8.
pub(super) const EXPAND_FACTOR: usize = 2;

/// The largest message a saved buffer, a page, holds.
pub(super) const SAVED_MESSAGE_SIZE: usize = PAGE_SIZE - OVERHEAD;

/// Each CPU's saved buffer, if it has one, as `ipc_kmsg_cache`.
static CACHE: CpuLocal<Option<SavedBuffer>> = CpuLocal::new(None);

/// A saved buffer, a page, in a CPU's slot.
#[derive(Clone, Copy)]
struct SavedBuffer(NonNull<Buffer>);

// SAFETY: only its slot holds a saved buffer, whose bytes are plain
// integers; the CPU that takes it owns it alone.
unsafe impl Send for SavedBuffer {}

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
            (&raw mut (*buffer.as_ptr()).link).write(Link::new());
            (&raw mut (*buffer.as_ptr()).size).write(size);
        }
        Ok(Self(buffer))
    }

    /// The current CPU's saved one, taken from its slot, as
    /// `ikm_cache_alloc_try` does; `None` if the slot is empty.
    pub(super) fn take_cached() -> Option<Self> {
        CACHE.lock().take().map(|SavedBuffer(buffer)| Self(buffer))
    }

    /// One of the saved size, a page: the current CPU's saved one, or a new
    /// one, as `ikm_cache_alloc` does.
    ///
    /// # Errors
    ///
    /// As `allocate`.
    ///
    /// # Panics
    ///
    /// As `allocate`.
    pub fn allocate_saved() -> Result<Self, Error> {
        Self::take_cached().map_or_else(|| Self::allocate(PAGE_SIZE), Ok)
    }

    /// Saves it in the current CPU's slot if it is of the saved size and the
    /// slot is empty, else frees it, as `ikm_cache_free` does.
    pub fn cache_or_free(self) {
        let message = ManuallyDrop::new(self);
        // SAFETY: the buffer is ours alone.
        if unsafe { (*message.0.as_ptr()).size } == PAGE_SIZE {
            let mut slot = CACHE.lock();
            let empty = slot.is_none();
            if empty {
                *slot = Some(SavedBuffer(message.0));
            }
            drop(slot);
            if empty {
                return;
            }
        }
        // Freed outside the slot.
        drop(ManuallyDrop::into_inner(message));
    }

    /// Puts it last on `queue`, a port's, which takes it, as
    /// `ipc_kmsg_enqueue` does.
    pub(super) fn enqueue(self, queue: &mut TailQueue<'static, QueueAdapter>) {
        let message = ManuallyDrop::new(self);
        // SAFETY: the buffer is ours alone, given up to the queue.
        unsafe { queue.push_back_ptr(message.0) };
    }

    /// The message whose buffer `buffer` is, taken off its queue.
    pub(in crate::ipc) const fn from_queued(buffer: NonNull<Buffer>) -> Self {
        Self(buffer)
    }

    /// The first message on `queue`, a port's, taken off it, as
    /// `ipc_kmsg_rmqueue_first` does; `None` if it is empty.
    pub(super) fn dequeue(queue: &mut TailQueue<'static, QueueAdapter>) -> Option<Self> {
        queue.pop_front_ptr().map(Self)
    }

    /// Destroys it, as `ipc_kmsg_destroy` does: the rights its header
    /// carries are released, as `ipc_kmsg_clean` does, then its buffer is
    /// freed.
    ///
    /// # Panics
    ///
    /// For a send-once right, or a body that carries rights or memory: not
    /// yet. For any other type than a send right.
    pub(super) fn destroy(mut self) {
        let header = self.header_mut();
        let bits = header.bits;
        for (slot, message_type) in [
            (header.remote_port, bits.remote()),
            (header.local_port, bits.local()),
        ] {
            // IO_VALID: neither null nor dead.
            if matches!(slot, 0 | usize::MAX) {
                continue;
            }
            // SAFETY: a header's port slot holds the exposed address of a
            // reference to its port given up by `into_raw`, which the handle
            // takes; the message is freed below.
            let port = unsafe {
                Reference::from_raw(
                    NonNull::new(ptr::with_exposed_provenance_mut::<Port>(slot)).unwrap(),
                )
            };
            // As ipc_object_destroy.
            match message_type {
                Some(MessageType::PortSend) => Port::release_send(port),
                Some(MessageType::PortSendOnce) => {
                    panic!("ipc_object_destroy: ipc_notify_send_once, not yet")
                }
                _ => panic!("ipc_object_destroy: strange rights"),
            }
        }
        assert!(
            !bits.is_complex(),
            "ipc_kmsg_clean: ipc_kmsg_clean_body, not yet"
        );
        // As ikm_free.
        drop(self);
    }

    /// Its buffer's size, in bytes.
    pub(super) fn size(&self) -> usize {
        // SAFETY: the buffer is ours alone.
        unsafe { (*self.0.as_ptr()).size }
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

    /// Its message, read in place as a `T`.
    ///
    /// # Safety
    ///
    /// `T` is `#[repr(C)]`, a `Header` first, aligned to at most 8 bytes,
    /// and any bytes make one.
    ///
    /// # Panics
    ///
    /// If its buffer can't hold a `T`.
    pub unsafe fn layout<T>(&self) -> &T {
        // SAFETY: the caller's; the place is in the buffer, 8-aligned and
        // zeroed at allocation, and ours alone.
        unsafe { self.start::<T>().as_ref() }
    }

    /// Its message as a `T`, to change in place.
    ///
    /// # Safety
    ///
    /// As `layout`.
    ///
    /// # Panics
    ///
    /// As `layout`.
    pub unsafe fn layout_mut<T>(&mut self) -> &mut T {
        // SAFETY: as in `layout`.
        unsafe { self.start::<T>().as_mut() }
    }

    /// Its message as a `T`, to change in place, and the rest of its buffer
    /// as `E`s.
    ///
    /// # Safety
    ///
    /// As `layout`; `E` is aligned to at most 8 bytes, and any bytes make
    /// one.
    ///
    /// # Panics
    ///
    /// As `layout`.
    pub unsafe fn split_layout_mut<T, E>(&mut self) -> (&mut T, &[E]) {
        let mut start = self.start::<T>();
        let count = (self.size() - OVERHEAD - size_of::<T>()) / size_of::<E>();
        // SAFETY: as in `layout`, and the caller's for `E`; the `E`s fill the
        // buffer past the `T`, apart from it, 8-aligned as a `T`'s size is a
        // multiple of its alignment, 8 for its `Header`.
        unsafe {
            let rest = start.add(1).cast::<E>();
            (start.as_mut(), slice::from_raw_parts(rest.as_ptr(), count))
        }
    }

    /// Where its message starts, for a `T` there.
    ///
    /// # Panics
    ///
    /// If its buffer can't hold a `T`.
    fn start<T>(&self) -> NonNull<T> {
        // SAFETY: the buffer is ours alone.
        let size = unsafe { (*self.0.as_ptr()).size };
        assert!(
            size_of::<T>() <= size - OVERHEAD,
            "a message's layout past its buffer"
        );
        // SAFETY: the header's place, within the buffer.
        unsafe { self.0.byte_add(OVERHEAD) }.cast()
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

/// Copies the user's message at `address`, `size` bytes, into a kernel
/// message of the saved size, cached or new, as `ipc_kmsg_get` does; its
/// ports keep their names' 32 bits.
///
/// # Panics
///
/// If `size` is below a header or not a multiple of 8, if the message
/// might outgrow a saved buffer, if no buffer can be allocated, or if the
/// message can't be copied in: not yet.
pub fn get(address: usize, size: u32) -> KernelMessage {
    let length = usize::try_from(size).unwrap();
    assert!(
        length >= size_of::<Header>() && length.is_multiple_of(8),
        "ipc_kmsg_get: MACH_SEND_MSG_TOO_SMALL, not yet"
    );
    assert!(
        length * EXPAND_FACTOR <= SAVED_MESSAGE_SIZE,
        "ipc_kmsg_get: a buffer beyond a page, not yet"
    );
    let mut message = KernelMessage::allocate_saved()
        .unwrap_or_else(|_| panic!("ipc_kmsg_get: MACH_SEND_NO_BUFFER, not yet"));
    assert!(
        message.copy_in(address, size).is_ok(),
        "ipc_kmsg_get: MACH_SEND_INVALID_DATA, not yet"
    );
    message
}

/// Copies `size` bytes of `message`, header first, to the user's message at
/// `address`, then saves or frees it, as `ipc_kmsg_put` does.
///
/// # Panics
///
/// If its buffer can't hold `size` bytes. If the copy-out fails: not yet.
pub fn put(address: usize, mut message: KernelMessage, size: u32) {
    let length = usize::try_from(size).unwrap();
    let copied = arch::Current::copy_out(&message.message_mut()[..length], address);
    message.cache_or_free();
    assert!(
        copied.is_ok(),
        "ipc_kmsg_put: MACH_RCV_INVALID_DATA, not yet"
    );
}

impl Drop for KernelMessage {
    /// Frees its buffer, as `ikm_free` does.
    fn drop(&mut self) {
        // SAFETY: the buffer is ours alone.
        let size = unsafe { (*self.0.as_ptr()).size };
        // SAFETY: the buffer came from `kalloc::allocate(size)`, and is not
        // used again.
        unsafe { kalloc::free(self.0.cast(), size) };
    }
}
