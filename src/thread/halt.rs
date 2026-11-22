// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};
use core::sync::atomic::Ordering;

use crate::ast::Ast;
use crate::error::Error;
use crate::ipc::port::Port;
use crate::reference::Reference;
use crate::sched::{self, Preemption, WaitResult};
use crate::sync::CriticalSection;
use crate::{arch, smp};

use super::machine::MachineThread as _;
use super::state::ThreadState;
use super::thread::Thread;

impl Thread {
    /// Makes its port stand for nothing, as `ipc_thread_disable` does.
    fn disable_ipc(&self) {
        // SAFETY: the slot keeps its port's reference until the thread itself
        // halts (`terminate_ipc`), which this borrows: the current thread has
        // not, and another cannot run on CPU 0 before this returns.
        if let Some(port) = unsafe { self.self_port.load(Ordering::Acquire).as_ref() } {
            port.replace_kernel_object(None);
        }
    }

    /// Ends its ipc, as `ipc_thread_terminate` does: its slots emptied, its
    /// naked send rights released, its reply port given up and its port
    /// destroyed; the names tasks hold for the port stay, as rights to a dead
    /// port.
    ///
    /// # Panics
    ///
    /// If it has no port: not yet. As `Port::release_send`,
    /// `Port::orphan_reply` and `Port::deallocate_kernel`.
    pub(super) fn terminate_ipc(&self) {
        // Each slot is emptied before its reference goes: a later read finds
        // no port.
        let port = NonNull::new(self.self_port.swap(ptr::null_mut(), Ordering::AcqRel))
            .unwrap_or_else(|| panic!("ipc_thread_terminate: an ended thread, not yet"));
        // SAFETY: the slot kept the port's reference, given up to this handle.
        let port = unsafe { Reference::from_raw(port) };
        if let Some(send) = NonNull::new(self.self_send.swap(ptr::null_mut(), Ordering::AcqRel)) {
            // SAFETY: the slot kept the send right's reference, given up here.
            Port::release_send(unsafe { Reference::from_raw(send) });
        }
        let mut ports = self.ports.lock();
        let exception = ports.exception.take();
        let reply = ports.reply.take();
        drop(ports);
        // Released after the lock, as it drops.
        drop(exception);
        // Its reply port, given up beside them (SC-D2).
        if let Some(reply) = reply {
            Port::orphan_reply(reply);
        }
        port.deallocate_kernel();
        drop(port);
    }

    /// Ends it, as `thread_terminate` does: its port stands for nothing;
    /// the current thread, no longer alive, halts as it leaves the kernel;
    /// another is halted, gives up its ipc and the reference of its being
    /// alive.
    ///
    /// # Errors
    ///
    /// `Failure` if another thread is no longer alive.
    ///
    /// # Panics
    ///
    /// If no thread runs on the CPU. If the current task or thread is no
    /// longer alive: not yet. As `halt` and `terminate_ipc`.
    pub(crate) fn terminate(&self) -> Result<(), Error> {
        self.disable_ipc();
        let current = smp::current()
            .active_thread()
            .expect("thread_terminate: no current thread");
        if ptr::eq(current, self) {
            let section = CriticalSection::enter();
            if self.active.swap(false, Ordering::Relaxed) {
                self.add_asts(Ast::TERMINATE);
            }
            smp::current().add_asts(Ast::TERMINATE);
            drop(section);
            return Ok(());
        }
        let members = current.task().members.lock();
        assert!(
            members.active && current.active.load(Ordering::Relaxed),
            "thread_terminate: the current thread being ended, not yet"
        );
        drop(members);
        // Someone else got there first.
        if !self.active.swap(false, Ordering::Relaxed) {
            return Err(Error::Failure);
        }
        self.halt();
        self.terminate_ipc();
        // thread_deallocate: the reference of its being alive.
        // SAFETY: an alive thread holds that reference, given up here, once,
        // as it is no longer alive; the caller's keeps it until the call
        // ends.
        drop(unsafe { Reference::from_raw(NonNull::from(self)) });
        Ok(())
    }

    /// Halts it at a clean point, leaving it suspended, as `thread_halt`
    /// does when it must: once it stops, an interruptible wait it is in
    /// ends; never run and to start in user mode, it halts at once, else
    /// until it halts itself it is made to run, uninterruptibly, so that it
    /// notices.
    ///
    /// # Panics
    ///
    /// If it is the current thread, or, not halted, is anything but
    /// suspended once stopped. If another thread is halting it: not yet. As
    /// `wait_until_stopped`.
    pub(super) fn halt(&self) {
        assert!(
            !smp::current()
                .active_thread()
                .is_some_and(|current| ptr::eq(current, self)),
            "thread_halt: trying to halt current thread."
        );
        // SAFETY: the caller's reference keeps it alive through the call, and
        // the run queue holds it only until it stops, before the call returns.
        let thread = unsafe { &*ptr::from_ref(self) };
        let section = CriticalSection::enter();
        if self.state().contains(ThreadState::HALTED) {
            self.suspend_count.fetch_add(1, Ordering::Relaxed);
            drop(section);
            return;
        }
        self.hold();
        assert!(
            !self.asts().contains(Ast::HALT),
            "thread_halt: another thread halting it, not yet"
        );
        self.add_asts(Ast::HALT);
        drop(section);
        loop {
            self.wait_until_stopped();
            let section = CriticalSection::enter();
            // clear_wait's interrupt_only: an uninterruptible wait goes on.
            if !self.state().contains(ThreadState::UNINTERRUPTIBLE) {
                sched::clear_wait(thread, WaitResult::Interrupted);
            }
            if self.state().contains(ThreadState::HALTED) {
                drop(section);
                return;
            }
            // At a clean point, as at Mach's thread_bootstrap_return: never
            // run, and to start in user mode; re-mach has none of Mach's
            // other clean continuations.
            let user_mode: fn() -> ! = arch::MachineState::return_to_user;
            if self.last_processor().is_none()
                && self
                    .start_function
                    .get()
                    .is_some_and(|&start| ptr::fn_addr_eq(start, user_mode))
            {
                self.insert_state(ThreadState::HALTED);
                self.remove_asts(Ast::HALT);
                drop(section);
                return;
            }
            assert!(
                self.state() & ThreadState::SCHEDULING == ThreadState::SUSPENDED,
                "thread_halt"
            );
            self.insert_state(ThreadState::RUN | ThreadState::UNINTERRUPTIBLE);
            sched::make_runnable(thread, Preemption::Forbidden);
            drop(section);
        }
    }

    /// Whether it is to halt or end, as `thread_should_halt` tells.
    pub(crate) fn should_halt(&self) -> bool {
        self.asts().intersects(Ast::HALT | Ast::TERMINATE)
    }
}
