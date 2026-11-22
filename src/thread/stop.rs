// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr;
use core::sync::atomic::Ordering;

use crate::ast::Ast;
use crate::error::Error;
use crate::sched::{self, Event, Preemption};
use crate::smp;
use crate::sync::CriticalSection;

use super::state::ThreadState;
use super::thread::Thread;

impl Thread {
    /// Suspends it once more.
    pub(crate) fn hold(&self) {
        let _section = CriticalSection::enter();
        self.suspend_count.fetch_add(1, Ordering::Relaxed);
        self.state
            .fetch_or(ThreadState::SUSPENDED.bits(), Ordering::Relaxed);
    }

    /// Waits until it stops, as `thread_dowait` does: at once if it is
    /// suspended outside a wait or in an interruptible one, or if it is
    /// runnable and suspended, stopped as it leaves the current CPU's run
    /// queues; else it sleeps until the switch that stops it wakes it, and
    /// looks again.
    ///
    /// # Panics
    ///
    /// If it is the current thread. If it is runnable and suspended off the
    /// current CPU's run queues, or to wait for on another CPU: not yet.
    pub(super) fn wait_until_stopped(&self) {
        assert!(
            !smp::current()
                .active_thread()
                .is_some_and(|current| ptr::eq(current, self)),
            "thread_dowait"
        );
        let section = CriticalSection::enter();
        loop {
            // Acquire: what it did before its switch cleared RUN is seen.
            let state = ThreadState::from_bits_retain(self.state.load(Ordering::Acquire))
                & ThreadState::SCHEDULING;
            match state {
                state
                    if state == ThreadState::SUSPENDED
                        || state == ThreadState::WAIT | ThreadState::SUSPENDED =>
                {
                    break;
                }
                state if state == ThreadState::RUN | ThreadState::SUSPENDED => {
                    // The section keeps it from running until it is off the
                    // queue; not there, it runs on another CPU.
                    assert!(
                        smp::current().processor().remove(self),
                        "thread_dowait: cause_ast_check, not yet"
                    );
                    self.stop();
                    break;
                }
                state
                    if [
                        ThreadState::RUN | ThreadState::SUSPENDED | ThreadState::UNINTERRUPTIBLE,
                        ThreadState::RUN | ThreadState::WAIT | ThreadState::SUSPENDED,
                        ThreadState::RUN
                            | ThreadState::WAIT
                            | ThreadState::SUSPENDED
                            | ThreadState::UNINTERRUPTIBLE,
                        ThreadState::WAIT | ThreadState::SUSPENDED | ThreadState::UNINTERRUPTIBLE,
                    ]
                    .contains(&state) =>
                {
                    // Its wake comes from its own CPU's switch, on that CPU's
                    // sleep queue.
                    assert!(
                        self.last_processor().is_some_and(|processor| ptr::eq(
                            processor,
                            smp::current().processor()
                        )),
                        "thread_dowait: another CPU's thread, not yet"
                    );
                    self.wake_active.store(true, Ordering::Relaxed);
                    sched::assert_wait(Some(Event::of(&self.wake_active)));
                    sched::block();
                }
                // Matching no case, it counts as stopped.
                _ => break,
            }
        }
        drop(section);
    }

    /// Stops it, suspended, as its switch away stops it in
    /// `thread_dispatch`: it no longer runs, and the thread waiting for it
    /// to stop, if any, wakes.
    pub(crate) fn stop(&self) {
        // Release: what it did before is seen by `wait_until_stopped`'s
        // Acquire.
        self.state
            .fetch_and(!ThreadState::RUN.bits(), Ordering::Release);
        if self.wake_active.swap(false, Ordering::Relaxed) {
            sched::wakeup(Event::of(&self.wake_active));
        }
    }

    /// Lifts one suspension; a thread no longer suspended is no longer
    /// halted either, and runs unless it waits or already runs.
    ///
    /// # Panics
    ///
    /// As `sched::make_runnable`.
    pub(crate) fn release(&'static self) {
        let _section = CriticalSection::enter();
        if self.suspend_count.fetch_sub(1, Ordering::Relaxed) != 1 {
            return;
        }
        let state = self.state.fetch_and(
            !(ThreadState::SUSPENDED | ThreadState::HALTED).bits(),
            Ordering::Relaxed,
        );
        if state & (ThreadState::WAIT | ThreadState::RUN).bits() == 0 {
            self.state
                .fetch_or(ThreadState::RUN.bits(), Ordering::Relaxed);
            sched::make_runnable(self, Preemption::Allowed);
        }
    }

    /// Stops it once more for users, as `thread_suspend` does: the first
    /// stop suspends it, and the current thread blocks on its way out of the
    /// kernel; another is waited for until it stops.
    ///
    /// # Panics
    ///
    /// If it waits uninterruptibly: not yet. As `wait_until_stopped`.
    pub(crate) fn suspend(&self) {
        let section = CriticalSection::enter();
        assert!(
            !self.state().contains(ThreadState::UNINTERRUPTIBLE),
            "thread_suspend: TH_UNINT, not yet"
        );
        let first = self.user_stop_count.fetch_add(1, Ordering::Relaxed) == 0;
        if first {
            self.hold();
        }
        drop(section);
        if !first {
            return;
        }
        let section = CriticalSection::enter();
        let cpu = smp::current();
        if cpu
            .active_thread()
            .is_some_and(|current| ptr::eq(current, self))
        {
            cpu.add_asts(Ast::BLOCK);
        } else {
            drop(section);
            self.wait_until_stopped();
        }
    }

    /// Lifts one stop users asked for, and the suspension it holds; a thread
    /// no longer suspended at all is no longer halted either, and runs
    /// unless it waits.
    ///
    /// # Errors
    ///
    /// `Failure` if users have not stopped it.
    pub fn resume(&'static self) -> Result<(), Error> {
        if self.user_stop_count.load(Ordering::Relaxed) == 0 {
            return Err(Error::Failure);
        }
        if self.user_stop_count.fetch_sub(1, Ordering::Relaxed) == 1
            && self.suspend_count.fetch_sub(1, Ordering::Relaxed) == 1
        {
            let state = self.state.fetch_and(
                !(ThreadState::SUSPENDED | ThreadState::HALTED).bits(),
                Ordering::Relaxed,
            );
            if state & (ThreadState::WAIT | ThreadState::RUN).bits() == 0 {
                self.state
                    .fetch_or(ThreadState::RUN.bits(), Ordering::Relaxed);
                sched::make_runnable(self, Preemption::Allowed);
            }
        }
        Ok(())
    }

    /// Ends `thread`'s interruptible wait at a clean point, as
    /// `thread_abort` does: halted, then set going again, it runs unless
    /// still suspended.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for no thread or the current one.
    ///
    /// # Panics
    ///
    /// If the current thread is being halted, `thread` holds a kernel
    /// call's reply port or is depressed: not yet. As `halt` and `release`.
    pub(crate) fn abort(thread: Option<&'static Self>) -> Result<(), Error> {
        let current = smp::current().active_thread();
        let Some(thread) =
            thread.filter(|thread| !current.is_some_and(|current| ptr::eq(current, *thread)))
        else {
            return Err(Error::InvalidArgument);
        };
        // No event counter exists for evc_notify_abort.
        // thread_halt's cycle check, as it need not halt.
        assert!(
            thread.state().contains(ThreadState::HALTED)
                || !current.is_some_and(|current| current.asts().contains(Ast::HALT)),
            "thread_abort: KERN_ABORTED, not yet"
        );
        thread.halt();
        assert!(
            thread.ports.lock().reply.is_none(),
            "mach_msg_abort_rpc: ith_rpc_reply, not yet"
        );
        thread.release();
        assert!(
            thread.depressed_from.load(Ordering::Relaxed) < 0,
            "thread_abort: thread_depress_abort, not yet"
        );
        Ok(())
    }
}
