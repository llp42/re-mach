// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Exceptions: a thread's faults, sent to its exception port or its task's.

use crate::ipc::port::Port;
use crate::smp;
use crate::thread::Thread;

/// An exception, by Mach's `EXC_*` names; only those raised so far.
#[derive(Clone, Copy)]
pub enum Exception {
    /// `EXC_BAD_INSTRUCTION`: an instruction that failed.
    BadInstruction,
}

/// Sends the current thread's exception, with its code and subcode, to its
/// exception port, else its task's, as `exception` does.
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
/// If the task has no port. With no live exception port, or at
/// `exception_raise`: not yet. As `Thread::retrieve_self` and
/// `Task::retrieve_self`.
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
    // exception_raise's arguments.
    let _request = (
        port,
        thread.retrieve_self(),
        task.retrieve_self(),
        exception,
        code,
        subcode,
    );
    panic!("exception_raise: not yet");
}
