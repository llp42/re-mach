// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! New-task notifications, as `mach_notify_new_task` sends them.

use core::mem::size_of;

use crate::host::HOST;
use crate::ipc::MessageType;
use crate::ipc::kernel_message::KernelMessage;
use crate::ipc::kernel_message::header::{Header, MessageBits};
use crate::ipc::mach_msg;
use crate::ipc::port::port::Port;
use crate::ipc_kobject::mig::argument::type_word;
use crate::reference::Reference;

use super::task::Task;

/// `mach_notify_new_task`'s message id.
const REQUEST_ID: i32 = 4400;

/// Mach's type word for one port send right, inline.
const PORT_WORD: u64 = type_word(MessageType::PortSend, 64, 1);

/// The message a new task's notification carries: the child and its parent,
/// as send rights.
#[repr(C)]
struct Request {
    /// Its header.
    header: Header,
    /// The child port's type word.
    task_word: u64,
    /// The child port.
    task: usize,
    /// The parent port's type word.
    parent_word: u64,
    /// The parent port.
    parent: usize,
}

const _: () = assert!(size_of::<Request>() == 64);

/// Tells the registered notification port about `task`, which `parent`
/// made, as `mach_notify_new_task` does; does nothing while none is
/// registered.
///
/// # Panics
///
/// If no message buffer can be allocated or the notification port is dead:
/// not yet. As `KernelMessage::allocate_saved` and `mach_msg::send`.
pub fn new_task(task: Reference<Task>, parent: Reference<Task>) {
    let Some(notification) = HOST.new_task_notification() else {
        return;
    };
    // Its own send right for the message, as the client stub copies one.
    let destination = Port::copy_send(&notification);
    drop(notification);
    let mut message = KernelMessage::allocate_saved()
        .unwrap_or_else(|_| panic!("mach_notify_new_task: no buffer"));
    // SAFETY: `Request` is `repr(C)`, a `Header` first, then integers and
    // port handles only.
    let request = unsafe { message.layout_mut::<Request>() };
    *request = Request {
        header: Header {
            bits: MessageBits::new(Some(MessageType::PortSend), None).with_complex(),
            size: u32::try_from(size_of::<Request>()).unwrap(),
            remote_port: Reference::into_raw(destination)
                .as_ptr()
                .expose_provenance(),
            local_port: 0,
            sequence_number: 0,
            id: REQUEST_ID,
        },
        task_word: PORT_WORD,
        task: Reference::into_raw(Task::convert_to_port(task))
            .as_ptr()
            .expose_provenance(),
        parent_word: PORT_WORD,
        parent: Reference::into_raw(Task::convert_to_port(parent))
            .as_ptr()
            .expose_provenance(),
    };
    mach_msg::send::send_always(message);
}
