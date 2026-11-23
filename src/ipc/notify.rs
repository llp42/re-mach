// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The kernel's notifications, as `notify` sends them.

use core::mem::size_of;

use super::MessageType;
use super::kernel_message::KernelMessage;
use super::kernel_message::header::{Header, MessageBits};
use super::mach_msg;
use super::port::port::Port;
use super::port::port_name::PortName;
use crate::ipc_kobject::mig::argument::type_word;
use crate::reference::Reference;

/// `MACH_NOTIFY_PORT_DELETED`.
pub const PORT_DELETED: i32 = 0x41;

/// `MACH_NOTIFY_PORT_DESTROYED`.
pub const PORT_DESTROYED: i32 = 0x45;

/// `MACH_NOTIFY_NO_SENDERS`.
pub const NO_SENDERS: i32 = 0x46;

/// `MACH_NOTIFY_SEND_ONCE`.
pub const SEND_ONCE: i32 = 0x47;

/// `MACH_NOTIFY_DEAD_NAME`.
pub const DEAD_NAME: i32 = 0x48;

/// A notification naming a port: the header, the name's type word, the
/// name and padding.
#[repr(C)]
struct Named {
    /// Its header.
    header: Header,
    /// The name's type word.
    word: u64,
    /// The name.
    name: u32,
    /// Padding to 8 bytes.
    padding: u32,
}

const _: () = assert!(size_of::<Named>() == 48);

/// A no-senders notification: the header, the count's type word, the count
/// and padding.
#[repr(C)]
struct NoSenders {
    /// Its header.
    header: Header,
    /// The count's type word.
    word: u64,
    /// The port's make-send count.
    count: u32,
    /// Padding to 8 bytes.
    padding: u32,
}

const _: () = assert!(size_of::<NoSenders>() == 48);

/// A port-destroyed notification: the header, the receive right's type word
/// and the port.
#[repr(C)]
struct PortDestroyed {
    /// Its header.
    header: Header,
    /// The right's type word.
    word: u64,
    /// The receive right.
    port: usize,
}

const _: () = assert!(size_of::<PortDestroyed>() == 48);

/// Its header: to `notification`, a send-once right, `id`, `size` bytes,
/// complex if `complex`.
fn header(id: i32, notification: Reference<Port>, size: usize, complex: bool) -> Header {
    let bits = MessageBits::new(Some(MessageType::PortSendOnce), None);
    Header {
        bits: if complex { bits.with_complex() } else { bits },
        size: u32::try_from(size).unwrap(),
        remote_port: Reference::into_raw(notification)
            .as_ptr()
            .expose_provenance(),
        local_port: 0,
        sequence_number: 0,
        id,
    }
}

/// A message's buffer.
///
/// # Panics
///
/// If none can be allocated.
fn buffer() -> KernelMessage {
    KernelMessage::allocate_saved().unwrap_or_else(|_| panic!("mach_notify: no buffer"))
}

/// Sends `name`'s port-deleted notification to `notification`.
pub fn port_deleted(notification: Reference<Port>, name: PortName) {
    named(PORT_DELETED, notification, name);
}

/// Sends `name`'s dead-name notification to `notification`.
pub fn dead_name(notification: Reference<Port>, name: PortName) {
    named(DEAD_NAME, notification, name);
}

/// Sends `name` with the notification `id`, a port name, to `notification`.
fn named(id: i32, notification: Reference<Port>, name: PortName) {
    let mut message = buffer();
    // SAFETY: `Named` is `repr(C)`, a `Header`, then integers only.
    let notice = unsafe { message.layout_mut::<Named>() };
    *notice = Named {
        header: header(id, notification, size_of::<Named>(), false),
        word: type_word(MessageType::PortName, 32, 1),
        name: name.into(),
        padding: 0,
    };
    mach_msg::send::send_always(message);
}

/// Sends `mscount`'s no-senders notification to `notification`.
pub fn no_senders(notification: Reference<Port>, mscount: u32) {
    let mut message = buffer();
    // SAFETY: `NoSenders` is `repr(C)`, a `Header`, then integers only.
    let notice = unsafe { message.layout_mut::<NoSenders>() };
    *notice = NoSenders {
        header: header(NO_SENDERS, notification, size_of::<NoSenders>(), false),
        word: type_word(MessageType::Integer32, 32, 1),
        count: mscount,
        padding: 0,
    };
    mach_msg::send::send_always(message);
}

/// Sends a send-once notification to `notification`, as `ipc_notify_send_once`
/// does when a right it held goes.
pub fn send_once(notification: Reference<Port>) {
    let mut message = buffer();
    // SAFETY: the message's first bytes are a `Header`, as MIG lays it out.
    let head = unsafe { message.layout_mut::<Header>() };
    *head = header(SEND_ONCE, notification, size_of::<Header>(), false);
    mach_msg::send::send_always(message);
}

/// Sends the receive right `receive` to `notification`, as
/// `ipc_notify_port_destroyed` hands a destroyed port over.
pub fn port_destroyed(notification: Reference<Port>, receive: Reference<Port>) {
    let mut message = buffer();
    // SAFETY: `PortDestroyed` is `repr(C)`, a `Header`, then an integer.
    let notice = unsafe { message.layout_mut::<PortDestroyed>() };
    *notice = PortDestroyed {
        header: header(
            PORT_DESTROYED,
            notification,
            size_of::<PortDestroyed>(),
            true,
        ),
        word: type_word(MessageType::PortReceive, 64, 1),
        port: Reference::into_raw(receive).as_ptr().expose_provenance(),
    };
    mach_msg::send::send_always(message);
}
