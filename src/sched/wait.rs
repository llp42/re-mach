// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr;

use crate::clock;
use crate::config::HZ;
use crate::sched::event::Event;
use crate::sched::preemption::{Preemption, make_runnable};
use crate::sched::switch::block;
use crate::smp;
use crate::sync::{CriticalSection, SpinlockGuard};
use crate::thread::state::ThreadState;
use crate::thread::thread::Thread;

/// How a thread's wait ended, as Mach numbers it; only the results used
/// here.
pub enum WaitResult {
    /// It was woken.
    Awakened,
    /// Its timer ran out.
    TimedOut,
    /// It was interrupted.
    Interrupted,
}

impl WaitResult {
    /// Its number.
    pub(crate) const fn number(self) -> u8 {
        match self {
            Self::Awakened => 0,
            Self::TimedOut => 1,
            Self::Interrupted => 2,
        }
    }
}

impl TryFrom<u8> for WaitResult {
    type Error = u8;

    /// The result of Mach's number `number`, if it is one used here.
    fn try_from(number: u8) -> Result<Self, u8> {
        match number {
            0 => Ok(Self::Awakened),
            1 => Ok(Self::TimedOut),
            2 => Ok(Self::Interrupted),
            _ => Err(number),
        }
    }
}

/// Makes the current thread wait, uninterruptibly, for `event` or for none:
/// with an event it goes on its CPU's sleep queue for it, and it stops
/// running once it blocks.
///
/// # Panics
///
/// If it waits for an event already.
pub fn assert_wait(event: Option<Event>) {
    let _section = CriticalSection::enter();
    let cpu = smp::current();
    let thread = cpu.active_thread().expect("assert_wait: no current thread");
    if let Some(waited) = thread.wait_event() {
        panic!(
            "assert_wait: already asserted event {:#x}",
            usize::from(waited)
        );
    }
    if let Some(event) = event {
        cpu.sleep_queues()[event.hash()].push_back(thread);
        thread.set_wait_event(Some(event));
    }
    thread.insert_state(ThreadState::WAIT | ThreadState::UNINTERRUPTIBLE);
}

/// Makes the current thread wait, interruptibly, for no event: it stops
/// running once it blocks.
///
/// # Panics
///
/// If no thread runs on the CPU.
pub fn will_wait() {
    let _section = CriticalSection::enter();
    smp::current()
        .active_thread()
        .expect("thread_will_wait: no current thread")
        .insert_state(ThreadState::WAIT);
}

/// As `will_wait`, with the thread's timer set to end the wait
/// `milliseconds` later, rounded up to ticks.
///
/// # Panics
///
/// If no thread runs on the CPU. As `set_timeout`.
pub fn will_wait_with_timeout(milliseconds: u32) {
    let _section = CriticalSection::enter();
    let thread = smp::current()
        .active_thread()
        .expect("thread_will_wait_with_timeout: no current thread");
    thread.insert_state(ThreadState::WAIT);
    let ticks = (u64::from(milliseconds) * u64::from(HZ)).div_ceil(1000);
    clock::timeout::set_timeout(thread.timer(), ticks);
}

/// Makes the current thread wait for `event`, releases `guard`'s lock and
/// blocks; returns once the thread runs again.
pub fn sleep<T>(event: Event, guard: SpinlockGuard<'_, T>) {
    assert_wait(Some(event));
    drop(guard);
    block();
}

/// Wakes the threads waiting for `event` on the current CPU's sleep queue:
/// each leaves it, awakened, and runs.
///
/// # Panics
///
/// If one runs already, or is suspended in an interruptible wait: not yet.
pub fn wakeup(event: Event) {
    let _section = CriticalSection::enter();
    let mut queues = smp::current().sleep_queues();
    let mut cursor = queues[event.hash()].front_mut();
    while let Some(thread) = cursor.get() {
        if thread.wait_event() != Some(event) {
            cursor.move_next();
            continue;
        }
        let thread = cursor.remove().unwrap();
        thread.set_wait_event(None);
        clock::timeout::reset_timeout_check(thread.timer());
        let state = thread.state();
        assert!(
            [
                ThreadState::WAIT | ThreadState::SUSPENDED | ThreadState::UNINTERRUPTIBLE,
                ThreadState::WAIT | ThreadState::UNINTERRUPTIBLE,
                ThreadState::WAIT,
            ]
            .contains(&state),
            "thread_wakeup_prim: thread {thread:p} in state {:#x}, not yet",
            state.bits()
        );
        thread.set_state((state - ThreadState::WAIT) | ThreadState::RUN);
        thread.set_wait_result(WaitResult::Awakened);
        make_runnable(thread, Preemption::Allowed);
    }
}

/// Cancels `thread`'s timer and ends its wait with `result`, on its home
/// CPU: it leaves the sleep queue of its event and runs, unless suspended
/// in an interruptible wait, where it stays stopped; a thread not waiting
/// is left as it is.
///
/// # Panics
///
/// If it runs already, or waits on another CPU's sleep queue: not yet.
pub fn clear_wait(thread: &'static Thread, result: WaitResult) {
    let _section = CriticalSection::enter();
    if let Some(event) = thread.wait_event() {
        let cpu = smp::current();
        assert!(
            thread
                .last_processor()
                .is_some_and(|processor| ptr::eq(processor, cpu.processor())),
            "clear_wait: another CPU's thread, not yet"
        );
        let mut queues = cpu.sleep_queues();
        // SAFETY: a waiting thread is on its last CPU's queue for its event,
        // this CPU by the check above.
        unsafe { queues[event.hash()].cursor_mut_from_ptr(thread) }.remove();
        drop(queues);
        thread.set_wait_event(None);
    }
    clock::timeout::reset_timeout_check(thread.timer());
    let state = thread.state();
    if !state.contains(ThreadState::WAIT) {
        return;
    }
    if state == ThreadState::WAIT | ThreadState::SUSPENDED {
        // Its wait ends; suspended, it does not run.
        thread.set_state(state - ThreadState::WAIT);
        thread.set_wait_result(result);
        return;
    }
    assert!(
        [
            ThreadState::WAIT | ThreadState::SUSPENDED | ThreadState::UNINTERRUPTIBLE,
            ThreadState::WAIT | ThreadState::UNINTERRUPTIBLE,
            ThreadState::WAIT,
        ]
        .contains(&state),
        "clear_wait: thread {thread:p} in state {:#x}, not yet",
        state.bits()
    );
    thread.set_state((state - ThreadState::WAIT) | ThreadState::RUN);
    thread.set_wait_result(result);
    make_runnable(thread, Preemption::Allowed);
}
