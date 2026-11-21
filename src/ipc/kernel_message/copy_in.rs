// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Copying a message in: from the user, then the sender's rights and memory.

use core::ptr;

use super::KernelMessage;
use super::header::Header;
use crate::arch::{self, Machine as _};
use crate::error::Error;
use crate::ipc::port::{KernelObject, Port, PortName};
use crate::ipc::space::Space;
use crate::ipc::{MessageType, object};
use crate::reference::Reference;
use crate::slab::CacheBox;
use crate::vm::VirtualAddress;
use crate::vm::map::{Map, Source};

impl KernelMessage {
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
    pub(in crate::ipc) fn copy_in(&mut self, address: usize, size: u32) -> Result<(), Error> {
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
    pub(in crate::ipc) fn copy_in_body(&mut self, space: &Space, map: &Map) {
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
}
