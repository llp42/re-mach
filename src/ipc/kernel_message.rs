// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Kernel messages: messages in kernel buffers.

use core::mem::{ManuallyDrop, offset_of};
use core::ptr::{self, NonNull};
use core::slice;

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::object::{self, Object};
use super::port::{KernelObject, Port, PortName};
use super::space::Space;
use super::{MessageType, right};
use crate::arch::{self, Machine as _, PAGE_SIZE};
use crate::error::Error;
use crate::kalloc;
use crate::reference::Reference;
use crate::slab::CacheBox;
use crate::sync::CpuLocal;
use crate::vm::map::{COPY_CACHE, Map, MapCopy, Placement, Source};
use crate::vm::{self, VirtualAddress};

/// A message's header, in Mach's layout. Its ports are names as the user
/// sent them, then, once copied in, ports' exposed addresses.
#[repr(C)]
pub struct Header {
    /// Its ports' right types, and whether it is complex.
    pub bits: MessageBits,
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

/// A message header's bits, as Mach's: its ports' right types, and whether
/// it is complex.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MessageBits(u32);

impl MessageBits {
    /// The bit of a message whose body holds rights or out-of-line memory,
    /// `MACH_MSGH_BITS_COMPLEX`.
    const COMPLEX: u32 = 1 << 31;

    /// The bits of a simple message with right types `remote` for its
    /// destination and `local` for its reply port, as `MACH_MSGH_BITS` makes
    /// them; `None`, type 0, for no port.
    pub const fn new(remote: Option<MessageType>, local: Option<MessageType>) -> Self {
        let remote = match remote {
            Some(message_type) => message_type.number(),
            None => 0,
        };
        let local = match local {
            Some(message_type) => message_type.number(),
            None => 0,
        };
        Self(u32::from_le_bytes([remote, local, 0, 0]))
    }

    /// Its destination's right type, as `MACH_MSGH_BITS_REMOTE` gives it, if
    /// it has one.
    ///
    /// # Panics
    ///
    /// If the type is not one used here: not yet.
    pub fn remote(self) -> Option<MessageType> {
        let [remote, ..] = self.0.to_le_bytes();
        (remote != 0).then(|| {
            MessageType::try_from(remote)
                .unwrap_or_else(|number| panic!("MACH_MSGH_BITS_REMOTE: type {number}, not yet"))
        })
    }

    /// Its reply port's right type, as `MACH_MSGH_BITS_LOCAL` gives it, if
    /// it has one.
    ///
    /// # Panics
    ///
    /// If the type is not one used here: not yet.
    pub fn local(self) -> Option<MessageType> {
        let [_, local, ..] = self.0.to_le_bytes();
        (local != 0).then(|| {
            MessageType::try_from(local)
                .unwrap_or_else(|number| panic!("MACH_MSGH_BITS_LOCAL: type {number}, not yet"))
        })
    }

    /// Whether its message's body holds rights or out-of-line memory.
    pub const fn is_complex(self) -> bool {
        self.0 & Self::COMPLEX != 0
    }

    /// The same bits, complex.
    pub const fn with_complex(self) -> Self {
        Self(self.0 | Self::COMPLEX)
    }
}

impl From<MessageBits> for u32 {
    fn from(bits: MessageBits) -> Self {
        bits.0
    }
}

/// A kernel message: its buffer, from kalloc.
pub struct KernelMessage(NonNull<Buffer>);

/// A kernel message's buffer: its place in a port's queue, its size, then
/// the message, header first.
#[repr(C)]
pub(super) struct Buffer {
    /// Its place in the queue of the port it waits on.
    link: LinkedListAtomicLink,
    /// Its size, in bytes.
    size: usize,
    /// The message's header.
    header: Header,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub(super) QueueAdapter = UnsafeRef<Buffer>: Buffer { link => LinkedListAtomicLink }
);

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
            (&raw mut (*buffer.as_ptr()).link).write(LinkedListAtomicLink::new());
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
    pub(super) fn enqueue(self, queue: &mut LinkedList<QueueAdapter>) {
        let message = ManuallyDrop::new(self);
        // SAFETY: the buffer is ours alone, given up to the queue.
        queue.push_back(unsafe { UnsafeRef::from_raw(message.0.as_ptr()) });
    }

    /// The first message on `queue`, a port's, taken off it, as
    /// `ipc_kmsg_rmqueue_first` does; `None` if it is empty.
    pub(super) fn dequeue(queue: &mut LinkedList<QueueAdapter>) -> Option<Self> {
        queue
            .pop_front()
            .map(|buffer| Self(NonNull::new(UnsafeRef::into_raw(buffer)).unwrap()))
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

    /// Copies the user's message at `address`, `size` bytes, into it, as
    /// `copyinmsg` does: its size is set, and its ports keep their names'
    /// 32 bits.
    ///
    /// # Errors
    ///
    /// As `Machine::copy_in`.
    ///
    /// # Panics
    ///
    /// If its buffer can't hold `size` bytes.
    pub(super) fn copy_in(&mut self, address: usize, size: u32) -> Result<(), Error> {
        let length = usize::try_from(size).unwrap();
        arch::Current::copy_in(address, &mut self.message_mut()[..length])?;
        let header = self.header_mut();
        header.size = size;
        header.remote_port &= 0xffff_ffff;
        header.local_port &= 0xffff_ffff;
        Ok(())
    }

    /// Copies in its body's rights from `space` and its out-of-line memory
    /// from `map`, the sender's, as `ipc_kmsg_copyin_body` does: each inline
    /// port's name, its 32 bits, becomes the exposed address of its port,
    /// which the message's right holds, a null name a null port; out-of-line
    /// memory is copied in, its source kept, and the copy's exposed address
    /// replaces its address.
    ///
    /// # Panics
    ///
    /// If a type is invalid or the body too small, ports are out of line,
    /// out-of-line memory is empty or to be deallocated, or goes to a port
    /// that takes it as page lists, a right is not a copied or made send
    /// right, a name is dead, or no item carries a right or memory: not
    /// yet. As `object::copy_in` and `Map::copy_in`.
    pub(super) fn copy_in_body(&mut self, space: &Space, map: &Map) {
        // SAFETY: copied in, the destination is a port's exposed address, and
        // the message holds a reference to it.
        let destination =
            unsafe { &*ptr::with_exposed_provenance::<Port>(self.header().remote_port) };
        // ipc_kobject_vm_page_list: paging and device ports.
        let page_lists = matches!(
            destination.kernel_object(),
            Some(KernelObject::PagingRequest(_) | KernelObject::Device(_))
        );
        let end = usize::try_from(self.header().size).unwrap();
        let message = self.message_mut();
        let mut offset = size_of::<Header>();
        let mut complex = false;
        while offset < end {
            // Short and long type words are both 8 bytes here.
            assert!(
                end - offset >= 8,
                "ipc_kmsg_copyin_body: MACH_SEND_MSG_TOO_SMALL, not yet"
            );
            let place = offset;
            let mut word = u64::from_le_bytes(message[place..place + 8].try_into().unwrap());
            offset += 8;
            let [name, size_low, size_high, ..] = word.to_le_bytes();
            let size = u16::from_le_bytes([size_low, size_high]);
            let number = word >> 32;
            // msgt_inline, bit 29.
            let inline = word & (1 << 29) != 0;
            // MACH_MSG_TYPE_PORT_ANY: from MOVE_RECEIVE to MAKE_SEND_ONCE.
            let port = (16..=21).contains(&name);
            // A port's size, as a name out of line, as a port inline; msgt_unused,
            // bits 24 to 28; msgt_deallocate, bit 31, only out of line.
            assert!(
                !(port && size != if inline { 64 } else { 32 })
                    && word & (0x1f << 24) == 0
                    && !(inline && word & (1 << 31) != 0),
                "ipc_kmsg_copyin_body: MACH_SEND_INVALID_TYPE, not yet"
            );
            let length = usize::try_from((number * u64::from(size)).div_ceil(8)).unwrap();
            // The data inline, else its address.
            assert!(
                end - offset >= if inline { length } else { 8 },
                "ipc_kmsg_copyin_body: MACH_SEND_MSG_TOO_SMALL, not yet"
            );
            let data = offset;
            if inline {
                offset += length;
            } else {
                assert!(!port, "ipc_kmsg_copyin_body: out-of-line ports, not yet");
                let address = usize::from_le_bytes(message[data..data + 8].try_into().unwrap());
                assert!(
                    length != 0,
                    "ipc_kmsg_copyin_body: empty out-of-line memory, not yet"
                );
                assert!(
                    !page_lists,
                    "ipc_kmsg_copyin_body: vm_map_copyin_page_list, not yet"
                );
                // msgt_deallocate, bit 31.
                assert!(
                    word & (1 << 31) == 0,
                    "ipc_kmsg_copyin_body: vm_map_copyin destroying its source, not yet"
                );
                let copy = map.copy_in(
                    VirtualAddress::from(address)
                        ..VirtualAddress::from(address.wrapping_add(length)),
                    Source::Keep,
                );
                let copy = CacheBox::into_raw(copy).as_ptr().expose_provenance();
                message[data..data + 8].copy_from_slice(&copy.to_le_bytes());
                offset += 8;
                complex = true;
            }
            if port {
                let Ok(message_type @ (MessageType::CopySend | MessageType::MakeSend)) =
                    MessageType::try_from(name)
                else {
                    panic!(
                        "ipc_kmsg_copyin_body: type {name}, MOVE_ and MAKE_SEND_ONCE rights, not yet"
                    );
                };
                // As ipc_object_copyin_type.
                word = (word & !0xff) | u64::from(MessageType::PortSend.number());
                message[place..place + 8].copy_from_slice(&word.to_le_bytes());
                for slot in message[data..offset].as_chunks_mut::<8>().0 {
                    // Only a name's 32 bits count.
                    let port_name =
                        PortName::from(u32::from_le_bytes(slot[..4].try_into().unwrap()));
                    let address = if port_name.is_valid() {
                        let copied = object::copy_in(space, port_name, message_type);
                        Reference::into_raw(copied).as_ptr().expose_provenance()
                    } else {
                        assert!(
                            port_name == PortName::NULL,
                            "ipc_kmsg_copyin_body: invalid_name_to_port of MACH_PORT_DEAD, not yet"
                        );
                        0
                    };
                    *slot = address.to_le_bytes();
                }
                complex = true;
            }
            offset = offset.next_multiple_of(8);
        }
        assert!(
            complex,
            "ipc_kmsg_copyin_body: no rights or memory, MACH_MSGH_BITS_COMPLEX cleared, not yet"
        );
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

    /// Copies out its header to `space`, its receiver, as
    /// `ipc_kmsg_copyout_header` does for a request: the reply port's
    /// send-once right moves into a new entry, whose name is its remote
    /// port; the destination's send right is consumed, its local port the
    /// space's name for the port, if it receives on it, else null; the right
    /// types swap. The space stays locked across both.
    ///
    /// # Panics
    ///
    /// For other right types, a dead space, reply port or destination, an
    /// entry that can't be allocated, or a protected payload: not yet. As
    /// `right::copy_out` and `object::copy_out_dest`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the ports' guards are moved into right::copy_out and object::copy_out_dest"
    )]
    pub(super) fn copy_out_header(&mut self, space: &Space) {
        let header = self.header_mut();
        let bits = header.bits;
        let request =
            MessageBits::new(Some(MessageType::PortSend), Some(MessageType::PortSendOnce));
        assert!(
            bits == request || bits == request.with_complex(),
            "ipc_kmsg_copyout_header: header bits {:#x}, not yet",
            u32::from(bits)
        );
        // SAFETY: copied in or made by the kernel, both slots hold the exposed
        // addresses of references given up by `into_raw`, which the handles
        // take, as the slots are overwritten below.
        let (destination, reply) = unsafe {
            (
                Reference::from_raw(
                    NonNull::new(ptr::with_exposed_provenance_mut::<Port>(header.remote_port))
                        .unwrap(),
                ),
                Reference::from_raw(
                    NonNull::new(ptr::with_exposed_provenance_mut::<Port>(header.local_port))
                        .unwrap(),
                ),
            )
        };
        let mut state = space.state.write();
        assert!(
            state.active,
            "ipc_kmsg_copyout_header: MACH_RCV_HEADER_ERROR|MACH_MSG_IPC_SPACE, not yet"
        );
        // A send-once right has a name of its own: no reverse lookup.
        let reply_name = match state.allocate() {
            Ok((name, _)) => name,
            Err(error) => {
                panic!("ipc_kmsg_copyout_header: ipc_entry_alloc: {error:?}, not yet")
            }
        };
        let reply_state = reply.state.lock();
        assert!(
            reply_state.active,
            "ipc_kmsg_copyout_header: a dead reply port, not yet"
        );
        // The entry takes the right's reference.
        let entry = right::copy_out(
            &mut state,
            reply_name,
            MessageType::PortSendOnce,
            &reply,
            reply_state,
        )
        .expect("ipc_kmsg_copyout_header: a send-once right merged");
        entry.object = Some(Object::Port(reply));
        // Locked before the space is unlocked, so its receive right can't move
        // between the two copy-outs.
        let destination_state = destination.state.lock();
        drop(state);
        assert!(
            destination_state.active,
            "ipc_kmsg_copyout_header: a dead destination, not yet"
        );
        assert!(
            destination_state.protected_payload.is_none(),
            "ipc_kmsg_copyout_header: MACH_MSG_TYPE_PROTECTED_PAYLOAD, not yet"
        );
        let destination_name =
            object::copy_out_dest(space, destination_state, MessageType::PortSend);
        // The right's reference, as ipc_object_copyout_dest's io_release.
        drop(destination);
        let swapped =
            MessageBits::new(Some(MessageType::PortSendOnce), Some(MessageType::PortSend));
        header.bits = if bits.is_complex() {
            swapped.with_complex()
        } else {
            swapped
        };
        // The names, zero-extended.
        header.remote_port = usize::try_from(u32::from(reply_name)).unwrap();
        header.local_port = usize::try_from(u32::from(destination_name)).unwrap();
    }

    /// Copies its body out to its receiver, as `ipc_kmsg_copyout_body`
    /// does: each send right moves into `space`, and its name there replaces
    /// its port inline, or goes out of line to memory allocated in `map`,
    /// whose address replaces the ports' array; out-of-line memory moves
    /// into `map`, and its address replaces its copy; empty out-of-line data
    /// gets address 0.
    ///
    /// # Panics
    ///
    /// If empty out-of-line data has an address. If an item carries rights
    /// other than send rights, a port is null or dead, a right can't be
    /// copied out, or memory can't be allocated or copied out: not yet. As
    /// `object::copy_out` and `Map::copy_out`.
    #[expect(
        clippy::too_many_lines,
        reason = "ipc_kmsg_copyout_body's steps, in GNU Mach's order"
    )]
    pub(super) fn copy_out_body(&mut self, space: &Space, map: &Map) {
        let end = usize::try_from(self.header().size).unwrap();
        let message = self.message_mut();
        let mut offset = size_of::<Header>();
        while offset < end {
            // Short and long type words are both 8 bytes here.
            let place = offset;
            let mut word = u64::from_le_bytes(message[place..place + 8].try_into().unwrap());
            offset += 8;
            let [name, size_low, size_high, ..] = word.to_le_bytes();
            let size = u16::from_le_bytes([size_low, size_high]);
            let number = word >> 32;
            let length = usize::try_from((number * u64::from(size)).div_ceil(8)).unwrap();
            // msgt_inline, bit 29.
            let inline = word & (1 << 29) != 0;
            let count = usize::try_from(number).unwrap();
            // Out-of-line ports' array, and the memory their names go to.
            let mut names = None;
            // MACH_MSG_TYPE_PORT_ANY: from MOVE_RECEIVE to MAKE_SEND_ONCE.
            if (16..=21).contains(&name) {
                assert!(
                    name == MessageType::PortSend.number(),
                    "ipc_kmsg_copyout_object: type {name}, not yet"
                );
                let slots = if inline {
                    message[offset..offset + length].as_chunks_mut::<8>().0
                } else {
                    let data =
                        usize::from_le_bytes(message[offset..offset + 8].try_into().unwrap());
                    if length != 0 {
                        // First the names' memory in `map`, 32 bits each.
                        let address =
                            vm::user::allocate(map, Placement::Anywhere, count * size_of::<u32>())
                                .unwrap_or_else(|error| {
                                    panic!(
                                        "ipc_kmsg_copyout_body: vm_allocate: {error:?}, vm_copyout_failure, not yet"
                                    )
                                });
                        let array = NonNull::new(ptr::with_exposed_provenance_mut::<[u8; 8]>(data))
                            .unwrap();
                        names = Some((array, address));
                    }
                    // Out-of-line ports go out as names: msgt_size 32.
                    word = (word & !(0xffff << 8)) | (32 << 8);
                    match names {
                        // SAFETY: out-of-line ports enter a message only through
                        // MIG's `PortArray::into_parts`: kalloc memory of
                        // `count` ports' exposed addresses, which the message
                        // alone holds.
                        Some((array, _)) => unsafe {
                            slice::from_raw_parts_mut(array.as_ptr(), count)
                        },
                        None => &mut [],
                    }
                };
                for slot in slots {
                    let address = usize::from_le_bytes(*slot);
                    assert!(
                        !matches!(address, 0 | usize::MAX),
                        "ipc_kmsg_copyout_object: invalid_port_to_name, not yet"
                    );
                    let port =
                        NonNull::new(ptr::with_exposed_provenance_mut::<Port>(address)).unwrap();
                    // SAFETY: a reply's port is the exposed address of a reference
                    // to it given up by `into_raw`, which the handle takes, as the
                    // slot is overwritten below.
                    let port = unsafe { Reference::from_raw(port) };
                    let port_name = object::copy_out(space, port, MessageType::PortSend)
                        .unwrap_or_else(|(error, _)| {
                            panic!(
                                "ipc_kmsg_copyout_object: {error:?}, ipc_object_destroy, not yet"
                            )
                        });
                    // The name, zero-extended.
                    *slot = usize::try_from(u32::from(port_name)).unwrap().to_le_bytes();
                }
            }
            // msgt_deallocate, bit 31.
            if inline {
                word &= !(1 << 31);
                offset += length;
            } else {
                let data = usize::from_le_bytes(message[offset..offset + 8].try_into().unwrap());
                let address = if length == 0 {
                    assert!(
                        data == 0,
                        "ipc_kmsg_copyout_body: empty out-of-line data with an address"
                    );
                    VirtualAddress::from(0)
                } else if let Some((array, address)) = names {
                    // SAFETY: the ports' array, as above, their names in it now.
                    let slots = unsafe { slice::from_raw_parts(array.as_ptr(), count) };
                    // Each name's 32 bits, as copyout_port.
                    for (slot, destination) in slots
                        .iter()
                        .zip((usize::from(address)..).step_by(size_of::<u32>()))
                    {
                        assert!(
                            arch::Current::copy_out(&slot[..size_of::<u32>()], destination).is_ok(),
                            "ipc_kmsg_copyout_body: copyout_port, vm_copyout_failure, not yet"
                        );
                    }
                    // SAFETY: from `kalloc::allocate(length)`, as
                    // `PortArray::new` asks, and not used again: the slot is
                    // overwritten below.
                    unsafe { kalloc::free(array.cast(), length) };
                    address
                } else {
                    let copy = NonNull::new(ptr::with_exposed_provenance_mut::<MapCopy>(data))
                        .expect("ipc_kmsg_copyout_body: VM_MAP_COPY_NULL, not yet");
                    // SAFETY: out-of-line data enters a message only through
                    // `copy_in_body` or MIG's `OutOfLine::into_parts`: the
                    // exposed address of a `COPY_CACHE` box from
                    // `CacheBox::into_raw`, which the message alone holds; the
                    // slot is overwritten below, so it is taken back once.
                    let copy = unsafe { CacheBox::from_raw(&COPY_CACHE, copy) };
                    match map.copy_out(copy) {
                        Ok(address) => address,
                        Err((_, error)) => panic!(
                            "ipc_kmsg_copyout_body: vm_map_copyout: {error:?}, vm_copyout_failure, not yet"
                        ),
                    }
                };
                word |= 1 << 31;
                message[offset..offset + 8].copy_from_slice(&usize::from(address).to_le_bytes());
                offset += 8;
            }
            message[place..place + 8].copy_from_slice(&word.to_le_bytes());
            offset = offset.next_multiple_of(8);
        }
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
