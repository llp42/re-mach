// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem;
use core::ptr::{self, NonNull};

use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::smp;

use super::members::TaskMembers;
use super::task::Task;

impl Task {
    /// Makes its port stand for nothing, as `ipc_task_disable` does.
    fn disable_ipc(&self) {
        let ports = self.ports.lock();
        if let Some(port) = &ports.self_port {
            port.replace_kernel_object(None);
        }
        drop(ports);
    }

    /// Ends its ipc, as `ipc_task_terminate` does: its slots emptied, its
    /// naked send rights released, its space destroyed and its port
    /// destroyed; the names tasks hold for the port stay, as rights to a dead
    /// port.
    ///
    /// # Panics
    ///
    /// If it has no port: not yet. As `Space::destroy` and
    /// `Port::deallocate_kernel`.
    fn terminate_ipc(&self) {
        let mut ports = self.ports.lock();
        let port = ports
            .self_port
            .take()
            .unwrap_or_else(|| panic!("ipc_task_terminate: an ended task, not yet"));
        let send = ports.self_send.take();
        let exception = ports.exception.take();
        let bootstrap = ports.bootstrap.take();
        let registered = mem::take(&mut ports.registered);
        drop(ports);
        // Released after the lock, in GNU Mach's order.
        if let Some(send) = send {
            Port::release_send(send);
        }
        drop(exception);
        drop(bootstrap);
        drop(registered);
        self.space.destroy();
        port.deallocate_kernel();
        drop(port);
    }

    /// Ends it, as `task_terminate` does for a task other than the current
    /// one: no longer alive, held and waited for, its port made to stand for
    /// nothing and its ipc ended, then its reference for being alive
    /// dropped; the caller's keeps it until the call ends.
    ///
    /// # Panics
    ///
    /// If no thread runs on the CPU. If it is the current task, it or the
    /// current task or thread is no longer alive, or it has threads: not
    /// yet. As `wait_until_stopped` and `terminate_ipc`.
    pub(crate) fn terminate(&self) {
        let current = smp::current()
            .active_thread()
            .expect("task_terminate: no current thread");
        assert!(
            !ptr::eq(current.task(), self),
            "task_terminate: the current task, not yet"
        );
        let current_members = current.task().members.lock();
        assert!(
            current_members.active && current.is_active(),
            "task_terminate: the current task or thread being ended, not yet"
        );
        drop(current_members);
        let mut members = self.members.lock();
        assert!(
            members.active,
            "task_terminate: a task being ended, not yet"
        );
        Self::hold(&mut members);
        members.active = false;
        drop(members);
        self.wait_until_stopped();
        self.disable_ipc();
        let members = self.members.lock();
        assert!(
            members.threads.is_empty(),
            "task_terminate: thread_force_terminate, not yet"
        );
        drop(members);
        self.terminate_ipc();
        // task_deallocate: the reference of its being alive.
        // SAFETY: an alive task holds that reference, given up here, once, as
        // it is no longer alive; the caller's keeps it until the call ends.
        drop(unsafe { Reference::from_raw(NonNull::from(self)) });
    }

    /// Lifts one suspension, releasing each of its threads once.
    ///
    /// # Panics
    ///
    /// If it is not alive: not yet. As `Thread::release`.
    pub(super) fn release(&self) {
        let mut members = self.members.lock();
        assert!(members.active, "task_release: an inactive task, not yet");
        members.suspend_count -= 1;
        for thread in members.threads.iter_ptr() {
            // SAFETY: `add_thread` takes only `&'static Thread`.
            unsafe { thread.as_ref() }.release();
        }
        drop(members);
    }

    /// Suspends the task of `members` once more, holding its threads but the
    /// current one, as `task_hold_locked` does.
    ///
    /// # Panics
    ///
    /// If it is not alive: not yet.
    pub(super) fn hold(members: &mut TaskMembers) {
        let current = smp::current()
            .active_thread()
            .expect("task_hold: no current thread");
        assert!(members.active, "task_hold: an inactive task, not yet");
        members.suspend_count += 1;
        for thread in &members.threads {
            if !ptr::eq(thread, current) {
                thread.hold();
            }
        }
    }

    /// Waits until its threads but the current one stop.
    ///
    /// # Panics
    ///
    /// If it is not alive, or has a thread other than the current one: not
    /// yet.
    pub(super) fn wait_until_stopped(&self) {
        let current = smp::current()
            .active_thread()
            .expect("task_dowait: no current thread");
        let members = self.members.lock();
        for thread in &members.threads {
            assert!(members.active, "task_dowait: an inactive task, not yet");
            assert!(
                ptr::eq(thread, current),
                "task_dowait: thread_dowait, not yet"
            );
        }
        drop(members);
    }
}
