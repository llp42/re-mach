// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Copying a message out: its rights and memory to the receiver.

use core::ptr::{self, NonNull};
use core::slice;

use super::KernelMessage;
use super::header::{Header, MessageBits};
use crate::arch::{self, Machine as _};
use crate::ipc::object::{self, Object};
use crate::ipc::port::Port;
use crate::ipc::space::Space;
use crate::ipc::{MessageType, right};
use crate::kalloc;
use crate::reference::Reference;
use crate::slab::CacheBox;
use crate::vm::map::{COPY_CACHE, Map, MapCopy, Placement};
use crate::vm::{self, VirtualAddress};

impl KernelMessage {
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
    pub(in crate::ipc) fn copy_out_header(&mut self, space: &Space) {
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
    pub(in crate::ipc) fn copy_out_body(&mut self, space: &Space, map: &Map) {
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
}
