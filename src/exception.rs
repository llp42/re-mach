// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Exceptions: a thread's faults, sent to its exception port or its task's.

use crate::ipc::MessageType;
use crate::ipc::kernel_message::{Header, KernelMessage, MessageBits};
use crate::ipc::mach_msg;
use crate::ipc::port::Port;
use crate::reference::Reference;
use crate::smp;
use crate::thread::Thread;

/// An exception, by Mach's `EXC_*` names; only those raised so far.
#[derive(Clone, Copy)]
pub enum Exception {
    /// `EXC_BAD_INSTRUCTION`: an instruction that failed.
    BadInstruction,
}

impl Exception {
    /// Its number, Mach's.
    const fn number(self) -> i32 {
        match self {
            Self::BadInstruction => 2,
        }
    }
}

/// `MACH_EXCEPTION_ID`: the request's id.
const REQUEST_ID: i32 = 2400;

/// `exc_port_proto`: the type word of one inline send right.
const PORT_WORD: u64 =
    u64::from_le_bytes([MessageType::PortSend.number(), 64, 0, 1 << 5, 1, 0, 0, 0]);

/// `exc_code_proto`: the type word of one inline 32-bit integer.
const CODE_WORD: u64 =
    u64::from_le_bytes([MessageType::Integer32.number(), 32, 0, 1 << 5, 1, 0, 0, 0]);

/// `exc_subcode_proto`: the type word of one inline 64-bit integer.
const SUBCODE_WORD: u64 =
    u64::from_le_bytes([MessageType::Integer64.number(), 64, 0, 1 << 5, 1, 0, 0, 0]);

/// The request an exception sends, as `struct mach_exception` lays it out:
/// each item's type word, then its data; the integers' holes are fields.
#[repr(C)]
struct Request {
    /// Its header.
    header: Header,
    /// The thread's type word.
    thread_word: u64,
    /// The thread's port, with a send right.
    thread: usize,
    /// The task's type word.
    task_word: u64,
    /// The task's port, with a send right.
    task: usize,
    /// The exception's type word.
    exception_word: u64,
    /// The exception's number.
    exception: i32,
    /// The hole after it.
    exception_hole: u32,
    /// The code's type word.
    code_word: u64,
    /// The code.
    code: i32,
    /// The hole after it.
    code_hole: u32,
    /// The subcode's type word.
    subcode_word: u64,
    /// The subcode.
    subcode: i64,
}

/// Sends the current thread's exception, with its code and subcode, to its
/// exception port, else its task's, as `exception` does; it returns if the
/// thread is to halt meanwhile.
///
/// # Panics
///
/// With no current thread. If the thread has an exception port: not yet.
/// As `try_task`.
pub fn raise(exception: Exception, code: i32, subcode: i64) {
    let thread = smp::current()
        .active_thread()
        .expect("exception: no current thread");
    // As retrieve_thread_exception.
    let ports = thread.ports.lock();
    assert!(
        ports.exception.is_none(),
        "exception: the thread's exception port, not yet"
    );
    drop(ports);
    try_task(thread, exception, code, subcode);
}

/// Sends `thread`'s exception to its task's exception port, as
/// `exception_try_task` does.
///
/// # Panics
///
/// If the task has no port. With no live exception port: not yet. As
/// `Thread::retrieve_self`, `Task::retrieve_self` and `send`.
fn try_task(thread: &Thread, exception: Exception, code: i32, subcode: i64) {
    let task = thread.task();
    // As retrieve_task_exception: a send right, if the port is alive.
    let ports = task.ports.lock();
    assert!(
        ports.self_port.is_some(),
        "exception_try_task: a task with no port"
    );
    let port = ports
        .exception
        .as_ref()
        .and_then(|port| Port::try_copy_send(port.as_ref()));
    drop(ports);
    let port = port.unwrap_or_else(|| panic!("exception_no_server: not yet"));
    send(
        thread,
        port,
        thread.retrieve_self(),
        task.retrieve_self(),
        exception,
        code,
        subcode,
    );
}

/// Sends `thread`'s exception, with its code and subcode, to the exception
/// port of send right `destination`, with send rights `thread_port` and
/// `task_port`, the three given with their references, then waits for the
/// reply on the thread's reply port, as `exception_raise` does by its slow
/// path; an interrupted wait returns if the thread is to halt.
///
/// # Panics
///
/// If no message buffer or reply port can be allocated. At the reply, or a
/// wait that ends otherwise: not yet. As `mach_msg::send_always` and
/// `mach_msg::receive_reply`.
fn send(
    thread: &Thread,
    destination: Reference<Port>,
    thread_port: Reference<Port>,
    task_port: Reference<Port>,
    exception: Exception,
    code: i32,
    subcode: i64,
) {
    // A buffer first, with nothing locked.
    let mut message =
        KernelMessage::allocate_saved().unwrap_or_else(|_| panic!("exception_raise: no buffer"));
    // The reply port, made on first use, allocated with the slot unlocked.
    let mut ports = thread.ports.lock();
    if ports.reply.is_none() {
        drop(ports);
        let port =
            Port::allocate_reply().unwrap_or_else(|_| panic!("exception_raise: no reply port"));
        ports = thread.ports.lock();
        assert!(
            ports.reply.is_none(),
            "exception_raise: a reply port made meanwhile"
        );
        ports.reply = Some(Reference::from(port));
    }
    let reply = ports.reply.as_ref().unwrap();
    // A send-once right for the server, and a reference to receive on, as
    // ith_port.
    let send_once = Port::make_send_once(reply);
    let saved = reply.clone();
    drop(ports);
    // SAFETY: `Request` is `repr(C)`, a `Header` first, then integers only.
    let request = unsafe { message.layout_mut::<Request>() };
    *request = Request {
        header: Header {
            bits: MessageBits::new(Some(MessageType::PortSend), Some(MessageType::PortSendOnce))
                .with_complex(),
            size: u32::try_from(size_of::<Request>()).unwrap(),
            // Each port's slot takes its right's reference.
            remote_port: Reference::into_raw(destination)
                .as_ptr()
                .expose_provenance(),
            local_port: Reference::into_raw(send_once).as_ptr().expose_provenance(),
            sequence_number: 0,
            id: REQUEST_ID,
        },
        thread_word: PORT_WORD,
        thread: Reference::into_raw(thread_port)
            .as_ptr()
            .expose_provenance(),
        task_word: PORT_WORD,
        task: Reference::into_raw(task_port).as_ptr().expose_provenance(),
        exception_word: CODE_WORD,
        exception: exception.number(),
        exception_hole: 0,
        code_word: CODE_WORD,
        code,
        code_hole: 0,
        subcode_word: SUBCODE_WORD,
        subcode,
    };
    mach_msg::send_always(message);
    // exception_raise_continue_slow: an interrupted wait goes on, unless the
    // thread is to halt: it returns then, the saved reference dropped, and
    // halts on its way out of the kernel.
    loop {
        match mach_msg::receive_reply(&saved) {
            Ok(_) => panic!("exception_raise_continue_slow: exception_parse_reply, not yet"),
            Err(mach_msg::MACH_RCV_INTERRUPTED) => {
                if thread.should_halt() {
                    return;
                }
            }
            Err(status) => panic!("exception_raise_continue_slow: {status:#x}, not yet"),
        }
    }
}
