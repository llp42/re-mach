// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr;

use crate::arch::{self, Machine as _};
use crate::ast::Ast;
use crate::collections::ArrayVec;
use crate::processor::{self, Processor, State};
use crate::sched::policy::{MIN_QUANTUM, Policy};
use crate::sched::preemption::{Preemption, make_runnable};
use crate::sched::priority::Priority;
use crate::smp;
use crate::sync::CriticalSection;
use crate::thread::state::ThreadState;
use crate::thread::thread::Thread;

/// Whether `thread`, running on `processor`, should give it up, as
/// `csw_needed` tells: it is suspended, a thread bound to the processor
/// waits, or past its first quantum a thread of its set waits at least as
/// high; a fixed-priority thread also gives way to a higher one in it.
pub fn switch_needed(thread: &Thread, processor: &Processor) -> bool {
    if thread.state().contains(ThreadState::SUSPENDED) || processor.bound_queued() != 0 {
        return true;
    }
    let Some(best) = processor
        .set()
        .filter(|set| set.queued() > 0)
        .and_then(processor::ProcessorSet::best_queued)
    else {
        return false;
    };
    let current = thread.scheduled_priority();
    let at_least_as_high = !processor.is_first_quantum() && !current.outranks(best);
    match thread.policy() {
        Policy::Timeshare => at_least_as_high,
        Policy::FixedPriority => at_least_as_high || best.outranks(current),
    }
}

/// Gives the current CPU up to the next thread to run, which may be the
/// current one; returns once the current one runs again.
///
/// # Panics
///
/// If the CPU holds a spinlock or a token, or is in an interrupt handler.
pub fn block() {
    let _section = CriticalSection::enter();
    let cpu = smp::current();
    assert!(cpu.spinlocks() == 0, "thread_block: spinlocks held");
    assert!(cpu.tokens() == 0, "thread_block: tokens held");
    assert!(!cpu.in_interrupt(), "thread_block: in an interrupt handler");
    cpu.remove_asts(Ast::BLOCK);
    let thread = cpu
        .active_thread()
        .expect("thread_block: no current thread");
    invoke(thread, select(cpu.processor(), thread));
}

/// The thread `processor`, the current CPU's, runs next after `current`,
/// with the processor's quantum set for it: a thread bound to it, else
/// another, else `current` if it may run on, else the processor's idle
/// thread, the processor then idle.
///
/// # Panics
///
/// If the processor is in no set, or must idle with no idle thread.
fn select(processor: &'static Processor, current: &'static Thread) -> &'static Thread {
    processor.start_first_quantum();
    let set = processor.set().expect("thread_select: no processor set");
    let thread = match processor.dequeue() {
        Some(thread) if thread.bound().is_some() => {
            processor.set_quantum(i32::try_from(MIN_QUANTUM).unwrap());
            return thread;
        }
        Some(thread) => thread,
        None if current.state() == ThreadState::RUN
            && current.is_in(set)
            && current
                .bound()
                .is_none_or(|bound| ptr::eq(bound, processor)) =>
        {
            current.catch_up();
            current
        }
        None => {
            if matches!(processor.state(), State::Running) {
                processor.set_state(State::Idle);
            }
            processor
                .idle_thread()
                .expect("choose_pset_thread: no idle thread")
        }
    };
    let quantum = thread.next_quantum(set.quantum());
    processor.set_quantum(i32::try_from(quantum).unwrap());
    thread
}

/// Makes `new` interruptible, then switches the current CPU to it from
/// `old`, the thread it runs, inside a critical section; returns once `old`
/// runs again, having dispatched the thread switched from then.
fn invoke(old: &'static Thread, new: &'static Thread) {
    new.remove_state(ThreadState::UNINTERRUPTIBLE);
    if ptr::eq(old, new) {
        return;
    }
    new.run_here();
    // The CPU trades the old thread's own ASTs for the new one's.
    let cpu = smp::current();
    cpu.remove_asts(Ast::HALT | Ast::TERMINATE);
    cpu.add_asts(new.asts());
    // SAFETY: the CPU runs on `old`'s stack; `new` was switched away from,
    // or got its stack before it was made runnable.
    dispatch(unsafe { old.switch(new) });
}

/// Puts `thread`, just switched from, where its state says: back on a run
/// queue if it runs on, nowhere if an idle thread; one suspended outside a
/// wait or in an interruptible one stops, halted or not, waking the thread
/// waiting for it to stop; one that waits otherwise stops running.
///
/// # Panics
///
/// If its state is not one of those.
pub fn dispatch(thread: &'static Thread) {
    match thread.state() {
        state if state == ThreadState::RUN => make_runnable(thread, Preemption::Forbidden),
        state
            if [
                ThreadState::RUN | ThreadState::SUSPENDED,
                ThreadState::RUN | ThreadState::SUSPENDED | ThreadState::HALTED,
                ThreadState::RUN | ThreadState::WAIT | ThreadState::SUSPENDED,
            ]
            .contains(&state) =>
        {
            thread.stop();
        }
        state
            if [
                ThreadState::RUN
                    | ThreadState::WAIT
                    | ThreadState::SUSPENDED
                    | ThreadState::UNINTERRUPTIBLE,
                ThreadState::RUN | ThreadState::WAIT | ThreadState::UNINTERRUPTIBLE,
                ThreadState::RUN | ThreadState::WAIT,
            ]
            .contains(&state) =>
        {
            thread.remove_state(ThreadState::RUN);
        }
        // Its processor chooses it when it has nothing else.
        state if state == ThreadState::RUN | ThreadState::IDLE => {}
        state => panic!(
            "thread_dispatch: thread {thread:p} in state {:#x}, not yet",
            state.bits()
        ),
    }
}

/// An idle thread's work, on the CPU of the processor it is bound to: it
/// takes the lowest priority and becomes the processor's idle thread, then
/// blocks; chosen again, it waits for runnable threads, then blocks.
///
/// # Panics
///
/// If its processor already has an idle thread, or is in a state other
/// than idle once threads are runnable.
pub fn idle_thread() -> ! {
    let section = CriticalSection::enter();
    let cpu = smp::current();
    let thread = cpu.active_thread().expect("idle_thread: no current thread");
    thread.set_priority(Priority::LOWEST);
    thread.set_scheduled_priority(Priority::LOWEST);
    thread.insert_state(ThreadState::IDLE);
    assert!(
        cpu.processor().set_idle_thread(thread).is_ok(),
        "idle_thread: twice"
    );
    drop(section);
    block();
    loop {
        // An interrupt that makes a thread runnable is held back until the
        // section ends, and wakes the CPU.
        let section = loop {
            let section = CriticalSection::enter();
            if smp::current().processor().queued() != 0 {
                break section;
            }
            arch::Current::idle();
            drop(section);
        };
        let processor = smp::current().processor();
        match processor.state() {
            State::Idle => {
                processor.set_state(State::Running);
                block();
            }
            state => panic!(
                "idle_thread: bad processor state {} on CPU {}",
                state.number(),
                smp::current().id()
            ),
        }
        drop(section);
    }
}

/// How many stuck threads a scan takes off the run queues at a time.
pub const MAX_STUCK_THREADS: usize = 16;

/// Catches up the stuck threads on the current CPU's run queues, each CPU
/// scanning its own: a pass takes up to `MAX_STUCK_THREADS` off the queues,
/// then makes runnable again each whose scheduling state is still only
/// `RUN`; passes go on while one finds no room.
pub(super) fn do_thread_scan() {
    loop {
        let mut stuck = ArrayVec::new();
        let section = CriticalSection::enter();
        let full = smp::current().processor().take_stuck(&mut stuck);
        drop(section);
        // The last taken goes back first, as in GNU Mach.
        for &thread in stuck.iter().rev() {
            let _section = CriticalSection::enter();
            if thread.state() & ThreadState::SCHEDULING == ThreadState::RUN {
                make_runnable(thread, Preemption::Allowed);
            }
        }
        if !full {
            return;
        }
    }
}
