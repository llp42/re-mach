// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The kernel's calls to pagers outside it, as `memory_object` and
//! `memory_object_default` send them.

use core::mem::size_of;

use crate::ipc::MessageType;
use crate::ipc::kernel_message::KernelMessage;
use crate::ipc::kernel_message::header::{Header, MessageBits};
use crate::ipc::mach_msg;
use crate::ipc::port::port::Port;
use crate::ipc_kobject::mig::argument::type_word;
use crate::reference::Reference;

/// `memory_object_create`'s message id.
const MEMORY_OBJECT_CREATE: i32 = 2250;

/// `memory_object_init`'s message id.
const MEMORY_OBJECT_INIT: i32 = 2200;

/// Its request: the new object's receive right, its size, the control and
/// name ports' send rights, and the page size.
#[repr(C)]
struct Create {
    /// Its header.
    header: Header,
    /// The object's type word.
    object_word: u64,
    /// Its receive right.
    object: usize,
    /// The size's type word.
    size_word: u64,
    /// Its size.
    size: usize,
    /// The control port's type word.
    control_word: u64,
    /// Its send right.
    control: usize,
    /// The name port's type word.
    name_word: u64,
    /// Its send right.
    name: usize,
    /// The page size's type word.
    page_word: u64,
    /// Its page size.
    page_size: usize,
}

const _: () = assert!(size_of::<Create>() == 112);

/// Its request: the control and name ports' send rights, and the page
/// size.
#[repr(C)]
struct Init {
    /// Its header.
    header: Header,
    /// The control port's type word.
    control_word: u64,
    /// Its send right.
    control: usize,
    /// The name port's type word.
    name_word: u64,
    /// Its send right.
    name: usize,
    /// The page size's type word.
    page_word: u64,
    /// Its page size.
    page_size: usize,
}

const _: () = assert!(size_of::<Init>() == 80);

/// Sends `memory_object_init` to `pager` for its object, with the `control`
/// and `name` ports' send rights and `page_size`.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn memory_object_init(
    pager: Reference<Port>,
    control: Reference<Port>,
    name: Reference<Port>,
    page_size: usize,
) {
    let mut message =
        KernelMessage::allocate_saved().unwrap_or_else(|_| panic!("memory_object_init: no buffer"));
    // SAFETY: `Init` is `repr(C)`, a `Header`, then integers and ports'
    // addresses only.
    let request = unsafe { message.layout_mut::<Init>() };
    let bits = MessageBits::new(Some(MessageType::PortSend), None).with_complex();
    *request = Init {
        header: Header {
            bits,
            size: u32::try_from(size_of::<Init>()).unwrap(),
            remote_port: Reference::into_raw(pager).as_ptr().expose_provenance(),
            local_port: 0,
            sequence_number: 0,
            id: MEMORY_OBJECT_INIT,
        },
        control_word: type_word(MessageType::PortSend, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        name_word: type_word(MessageType::PortSend, 64, 1),
        name: Reference::into_raw(name).as_ptr().expose_provenance(),
        page_word: type_word(MessageType::Integer64, 64, 1),
        page_size,
    };
    mach_msg::send::send_always(message);
}

/// Sends `memory_object_create` to `manager` for `object`, the new pager's
/// receive right, its `size`, the `control` and `name` ports' send rights
/// and `page_size`, as `vm_object_pager_create` tells the default manager.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn memory_object_create(
    manager: Reference<Port>,
    object: Reference<Port>,
    size: usize,
    control: Reference<Port>,
    name: Reference<Port>,
    page_size: usize,
) {
    let mut message = KernelMessage::allocate_saved()
        .unwrap_or_else(|_| panic!("memory_object_create: no buffer"));
    // SAFETY: `Create` is `repr(C)`, a `Header`, then integers and ports'
    // addresses only.
    let request = unsafe { message.layout_mut::<Create>() };
    let bits = MessageBits::new(Some(MessageType::PortSend), None).with_complex();
    *request = Create {
        header: Header {
            bits,
            size: u32::try_from(size_of::<Create>()).unwrap(),
            remote_port: Reference::into_raw(manager).as_ptr().expose_provenance(),
            local_port: 0,
            sequence_number: 0,
            id: MEMORY_OBJECT_CREATE,
        },
        object_word: type_word(MessageType::PortReceive, 64, 1),
        object: Reference::into_raw(object).as_ptr().expose_provenance(),
        size_word: type_word(MessageType::Integer64, 64, 1),
        size,
        control_word: type_word(MessageType::PortSend, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        name_word: type_word(MessageType::PortSend, 64, 1),
        name: Reference::into_raw(name).as_ptr().expose_provenance(),
        page_word: type_word(MessageType::Integer64, 64, 1),
        page_size,
    };
    mach_msg::send::send_always(message);
}
