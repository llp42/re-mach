// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::clock;
use crate::clock::timeout::Timeout;
use crate::sched;
use crate::sched::wait::WaitResult;
use crate::smp;
use crate::sync::CriticalSection;

use super::state::ThreadState;
use super::thread::Thread;

/// Ends the wait of the thread whose timer `timer` is, as timed out.
pub(super) fn thread_timeout(timer: &'static Timeout) {
    // SAFETY: only threads' timers are made with this function, each with
    // its thread as its argument, and a thread lives while its timer is set.
    let thread = unsafe { &*timer.argument().cast::<Thread>() };
    // Its home CPU is this one: the default set's threads run on CPU 0, and
    // the other CPUs' bound threads set no timer.
    sched::wait::clear_wait(thread, WaitResult::TimedOut);
}

/// Sets the current thread's timer to end its wait `ticks` ticks after the
/// next one, if it waits.
///
/// # Panics
///
/// If no thread runs on the CPU.
pub fn set_own_timeout(ticks: u64) {
    let _section = CriticalSection::enter();
    let thread = smp::current()
        .active_thread()
        .expect("thread_set_timeout: no current thread");
    if thread.state().contains(ThreadState::WAIT) {
        clock::timeout::set_timeout(&thread.timer, ticks);
    }
}
