// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem;
use core::ptr::{self, NonNull};
use core::sync::atomic::Ordering;

use crate::error::Error;
use crate::ipc::port::port::Port;
use crate::ipc::port::send_right::SendRight;
use crate::reference::Reference;

use super::thread::Thread;

/// A thread's ports under its lock, besides its own port; so far its
/// exception port and its reply port.
pub struct ThreadPorts {
    /// The send right its exceptions go to first, if any.
    pub(crate) exception: Option<SendRight>,
    /// The port it receives its kernel calls' replies on, made on first
    /// use; the slot holds the reference it was made with.
    pub(crate) reply: Option<Reference<Port>>,
}

/// A thread's special port, by Mach's `THREAD_*_PORT` number.
#[derive(Clone, Copy)]
pub enum SpecialPort {
    /// `THREAD_KERNEL_PORT`: the send right it hands out for its port.
    Kernel,
    /// `THREAD_EXCEPTION_PORT`: its exception port.
    Exception,
}

impl TryFrom<i32> for SpecialPort {
    type Error = i32;

    /// The special port Mach's number `number` names, if any.
    fn try_from(number: i32) -> Result<Self, i32> {
        match number {
            1 => Ok(Self::Kernel),
            3 => Ok(Self::Exception),
            _ => Err(number),
        }
    }
}

impl Thread {
    /// A send right for `thread`'s port, made as `convert_thread_to_port`
    /// makes it; `thread`'s reference drops after.
    ///
    /// # Panics
    ///
    /// If it has no port: not yet.
    pub(crate) fn convert_to_port(thread: Reference<Self>) -> Reference<Port> {
        let port = NonNull::new(thread.self_port.load(Ordering::Acquire))
            .unwrap_or_else(|| panic!("convert_thread_to_port: no port, not yet"));
        // SAFETY: the slot keeps its port's reference until the thread itself
        // halts (`terminate_ipc`), which this handle borrows and never drops:
        // up to `make_send`'s clone nothing blocks, and user threads share
        // CPU 0.
        let port = mem::ManuallyDrop::new(unsafe { Reference::from_raw(port) });
        let send = Port::make_send(&port);
        drop(thread);
        send
    }

    /// Gives a new send right for its port, with its reference, as
    /// `retrieve_thread_self_fast` does.
    ///
    /// # Panics
    ///
    /// If it has no port. If it hands out a right for another port: not
    /// yet. As `Port::copy_send`.
    pub(crate) fn retrieve_self(&self) -> Reference<Port> {
        let port = NonNull::new(self.self_port.load(Ordering::Acquire))
            .expect("retrieve_thread_self_fast: no thread port");
        assert!(
            ptr::eq(self.self_send.load(Ordering::Acquire), port.as_ptr()),
            "retrieve_thread_self_fast: an interposed port, not yet"
        );
        // SAFETY: the slot keeps its port's reference until the thread itself
        // halts (`terminate_ipc`), which this handle borrows and never drops:
        // the current thread halts only after its last trap.
        let port = mem::ManuallyDrop::new(unsafe { Reference::from_raw(port) });
        Port::copy_send(&port)
    }

    /// Sets its special port `which` to `port`, as `thread_set_special_port`
    /// does; the old right is released after its lock.
    ///
    /// # Errors
    ///
    /// `Failure` if it has no port; `port` is released.
    ///
    /// # Panics
    ///
    /// If `which` is its kernel port: not yet.
    pub(crate) fn set_special_port(
        &self,
        which: SpecialPort,
        port: Option<SendRight>,
    ) -> Result<(), Error> {
        let mut ports = self.ports.lock();
        // `terminate_ipc` empties its port before it takes the lock, then
        // releases what a slot holds.
        if self.self_port.load(Ordering::Acquire).is_null() {
            drop(ports);
            drop(port);
            return Err(Error::Failure);
        }
        let slot = match which {
            SpecialPort::Exception => &mut ports.exception,
            SpecialPort::Kernel => panic!("thread_set_special_port: THREAD_KERNEL_PORT, not yet"),
        };
        let old = mem::replace(slot, port);
        drop(ports);
        // As ipc_port_release_send.
        drop(old);
        Ok(())
    }
}
