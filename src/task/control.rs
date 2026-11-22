// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr;

use crate::ast::Ast;
use crate::error::Error;
use crate::ipc::port::{KernelObject, Port};
use crate::sched::priority::{BasePriority, Priority};
use crate::smp;
use crate::sync::CriticalSection;

use super::ports::Threads;
use super::task::Task;

impl Task {
    /// Stops it once more for users: the first stop suspends it, holding its
    /// threads and waiting for them to stop; the current thread, if one of
    /// them, is held and blocks on its way out of the kernel.
    ///
    /// # Panics
    ///
    /// If it is not alive, or has a thread other than the current one to
    /// wait for: not yet.
    pub(crate) fn suspend(&self) {
        let mut members = self.members.lock();
        members.user_stop_count += 1;
        let first = members.user_stop_count == 1;
        drop(members);
        if !first {
            return;
        }
        let mut members = self.members.lock();
        Self::hold(&mut members);
        drop(members);
        self.wait_until_stopped();
        let current = smp::current()
            .active_thread()
            .expect("task_suspend: no current thread");
        if ptr::eq(current.task(), self) {
            current.hold();
            let _section = CriticalSection::enter();
            smp::current().add_asts(Ast::BLOCK);
        }
    }

    /// Lifts one stop users asked for; the last releases it.
    ///
    /// # Errors
    ///
    /// `Failure` if users have not stopped it.
    ///
    /// # Panics
    ///
    /// As `release`.
    pub(crate) fn resume(&self) -> Result<(), Error> {
        let mut members = self.members.lock();
        if members.user_stop_count == 0 {
            return Err(Error::Failure);
        }
        members.user_stop_count -= 1;
        let release = members.user_stop_count == 0;
        drop(members);
        if release {
            self.release();
        }
        Ok(())
    }

    /// Sets its priority, which its new threads take, to `priority`, and its
    /// threads' as `threads` says, as `task_priority` does.
    ///
    /// # Errors
    ///
    /// `NoAccess` if `priority` outranks its cap; `Failure` if a thread's
    /// cap refuses it, the other threads changed still.
    ///
    /// # Panics
    ///
    /// As `Thread::try_set_priority`.
    pub(crate) fn set_priority(&self, priority: Priority, threads: Threads) -> Result<(), Error> {
        let mut members = self.members.lock();
        if priority.outranks(members.max_priority) {
            return Err(Error::NoAccess);
        }
        members.priority = priority;
        let mut result = Ok(());
        if matches!(threads, Threads::Changed) {
            for thread in &members.threads {
                if thread.try_set_priority(priority).is_err() {
                    result = Err(Error::Failure);
                }
            }
        }
        drop(members);
        result
    }

    /// Caps its priority at `max_priority` for `host`, a host's port: its
    /// priority as `base` says, and its threads' as `threads` says, as
    /// `task_max_priority` does.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` unless `host` is a host's port, alive; `NoAccess`
    /// if the cap is raised and `host` is not the privileged one.
    ///
    /// # Panics
    ///
    /// As `Thread::override_max_priority`.
    pub(crate) fn set_max_priority(
        &self,
        host: &Port,
        max_priority: Priority,
        base: BasePriority,
        threads: Threads,
    ) -> Result<(), Error> {
        let privileged = match host.kernel_object() {
            Some(KernelObject::Host(_)) => false,
            Some(KernelObject::HostPrivileged(_)) => true,
            _ => return Err(Error::InvalidArgument),
        };
        let mut members = self.members.lock();
        if max_priority.outranks(members.max_priority) && !privileged {
            return Err(Error::NoAccess);
        }
        members.max_priority = max_priority;
        if members.priority.outranks(max_priority) || matches!(base, BasePriority::Set) {
            members.priority = max_priority;
        }
        if matches!(threads, Threads::Changed) {
            for thread in &members.threads {
                thread.override_max_priority(max_priority, base);
            }
        }
        drop(members);
        Ok(())
    }

    /// Names it `name`, up to its first NUL, cut to fit.
    pub(crate) fn set_name(&self, name: impl IntoIterator<Item = u8>) {
        self.members.lock().name.set_bytes(name);
    }
}
