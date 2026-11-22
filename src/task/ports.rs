// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem;
use core::ptr;

use crate::ipc::port::port::Port;
use crate::ipc::port::send_right::SendRight;
use crate::reference::Reference;

use super::task::Task;

/// A task's ports, under its lock.
pub struct TaskPorts {
    /// Its port, which controls it.
    pub(crate) self_port: Option<Reference<Port>>,
    /// The send right for its port it hands out.
    pub(super) self_send: Option<Reference<Port>>,
    /// The send right its threads' exceptions go to, if any.
    pub(crate) exception: Option<SendRight>,
    /// The send right for its bootstrap port, if any.
    pub(super) bootstrap: Option<SendRight>,
    /// The send rights registered for it, Mach's `TASK_PORT_REGISTER_MAX`.
    pub(super) registered: [Option<SendRight>; 4],
}

/// What memory a new task starts with.
#[derive(Clone, Copy)]
pub enum Memory {
    /// A copy of its parent's, as each of the parent's entries' inheritance
    /// says.
    Inherited,
    /// None.
    Empty,
}

/// Whether a task's priority change reaches its threads.
#[derive(Clone, Copy)]
pub enum Threads {
    /// They keep theirs.
    Kept,
    /// They take it too.
    Changed,
}

/// A task's special port, by Mach's `TASK_*_PORT` number.
#[derive(Clone, Copy)]
pub enum SpecialPort {
    /// `TASK_KERNEL_PORT`: the send right it hands out for its port.
    Kernel,
    /// `TASK_EXCEPTION_PORT`: its exception port.
    Exception,
    /// `TASK_BOOTSTRAP_PORT`: its bootstrap port.
    Bootstrap,
}

impl TryFrom<i32> for SpecialPort {
    type Error = i32;

    /// The special port Mach's number `number` names, if any.
    fn try_from(number: i32) -> Result<Self, i32> {
        match number {
            1 => Ok(Self::Kernel),
            3 => Ok(Self::Exception),
            4 => Ok(Self::Bootstrap),
            _ => Err(number),
        }
    }
}

impl Task {
    /// A send right for `task`'s port, made as `convert_task_to_port` makes
    /// it; `task`'s reference drops after.
    ///
    /// # Panics
    ///
    /// If it has no port: not yet.
    pub(crate) fn convert_to_port(task: Reference<Self>) -> Reference<Port> {
        let ports = task.ports.lock();
        let port = ports
            .self_port
            .as_ref()
            .unwrap_or_else(|| panic!("convert_task_to_port: no port, not yet"));
        let send = Port::make_send(port);
        drop(ports);
        drop(task);
        send
    }

    /// Gives a new send right for its port, with its reference, as
    /// `retrieve_task_self_fast` does.
    ///
    /// # Panics
    ///
    /// If it has no port. If it hands out a right for another port: not
    /// yet. As `Port::copy_send`.
    pub(crate) fn retrieve_self(&self) -> Reference<Port> {
        let ports = self.ports.lock();
        let port = ports
            .self_port
            .as_ref()
            .expect("retrieve_task_self_fast: no task port");
        assert!(
            ports
                .self_send
                .as_deref()
                .is_some_and(|send| ptr::eq(send, ptr::from_ref::<Port>(port))),
            "retrieve_task_self_fast: an interposed port, not yet"
        );
        let send = Port::copy_send(port);
        drop(ports);
        send
    }

    /// Sets its special port `which` to `port`, as `task_set_special_port`
    /// does; the old right is released after its lock.
    ///
    /// # Panics
    ///
    /// If it has no port, or `which` is its kernel or bootstrap port: not
    /// yet.
    pub(crate) fn set_special_port(&self, which: SpecialPort, port: Option<SendRight>) {
        let mut ports = self.ports.lock();
        assert!(
            ports.self_port.is_some(),
            "task_set_special_port: KERN_FAILURE, not yet"
        );
        let slot = match which {
            SpecialPort::Exception => &mut ports.exception,
            SpecialPort::Kernel => panic!("task_set_special_port: TASK_KERNEL_PORT, not yet"),
            SpecialPort::Bootstrap => panic!("task_set_special_port: TASK_BOOTSTRAP_PORT, not yet"),
        };
        let old = mem::replace(slot, port);
        drop(ports);
        // As ipc_port_release_send.
        drop(old);
    }
}
