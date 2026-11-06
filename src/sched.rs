// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The scheduler.

use core::ptr;
use core::sync::atomic::{AtomicU32, Ordering};

use intrusive_collections::{LinkedList, UnsafeRef};

use crate::arch::{self, Machine as _};
use crate::ast::{self, Ast};
use crate::clock::{self, Timeout};
use crate::config::HZ;
use crate::machine::CpuState;
use crate::processor::{self, ActionQueueAdapter, Processor, State};
use crate::smp;
use crate::sync::{CriticalSection, SpinlockGuard};
use crate::thread::{RunQueueAdapter, SleepQueueAdapter, Thread, ThreadState};

/// The threads sleeping on the events that hash to one slot.
pub type SleepQueue = LinkedList<SleepQueueAdapter>;

/// How many sleep queues each CPU has.
pub const SLEEP_QUEUES: usize = 1031;

/// What a thread waits for: an address.
#[derive(Clone, Copy)]
pub struct Event(usize);

impl Event {
    /// The event of `object`'s address.
    pub fn of<T>(object: &T) -> Self {
        Self(ptr::from_ref(object).addr())
    }

    /// Its sleep queue: its address as a signed number, complemented if
    /// negative, modulo the queues.
    const fn hash(self) -> usize {
        let address = self.0.cast_signed();
        let positive = if address < 0 { !address } else { address };
        positive.cast_unsigned() % SLEEP_QUEUES
    }
}

/// The runnable threads of one priority.
pub type RunQueue = LinkedList<RunQueueAdapter>;

/// How many run queues each processor has, one per priority.
pub const RUN_QUEUES: usize = 65;

/// The processors waiting for the action thread to shut them down or move
/// them to another set.
#[expect(dead_code, reason = "processor_shutdown and the action thread use it")]
static ACTION_QUEUE: LinkedList<ActionQueueAdapter> = LinkedList::new(ActionQueueAdapter::NEW);

/// How many times threads' priorities have been recomputed, once a second.
static SCHEDULER_TICK: AtomicU32 = AtomicU32::new(0);

/// The scheduler tick.
pub fn scheduler_tick() -> u32 {
    SCHEDULER_TICK.load(Ordering::Relaxed)
}

/// What recomputes threads' priorities each second.
pub static RECOMPUTE_PRIORITIES: Timeout = Timeout::new(recompute_priorities);

/// Ages threads' use once more and sets itself again for a second later,
/// on the boot CPU, then once a second.
pub fn recompute_priorities(_: &'static Timeout) {
    SCHEDULER_TICK.fetch_add(1, Ordering::Relaxed);
    clock::set_timeout(&RECOMPUTE_PRIORITIES, u64::from(HZ));
}

/// The priority of system threads, and the default set's highest.
pub const SYSTEM_PRIORITY: u32 = 6;

/// The priority of user threads.
pub const USER_PRIORITY: u32 = 25;

/// The shortest quantum, in ticks: 33 context switches a second.
pub const MIN_QUANTUM: u32 = HZ / 33;

/// The scheduler's fixed-point one, which a set's load is scaled by.
pub const SCHEDULER_SCALE: u32 = 128;

/// How far `SCHEDULER_SCALE` shifts.
pub const SCHEDULER_SHIFT: u32 = 7;

/// How far a thread's use is shifted, past `SCHEDULER_SHIFT`, to age its
/// priority.
pub const PRIORITY_SHIFT: u32 = 17;

bitflags::bitflags! {
    /// Scheduling policies, as Mach's interface numbers them; only those
    /// used here.
    #[derive(PartialEq, Eq)]
    pub struct Policies: u32 {
        /// Priorities decay with use.
        const TIMESHARE = 1;
        /// Priorities stay, and each thread has its own quantum.
        const FIXED_PRIORITY = 2;
    }
}

/// How much scaled use a thread gathers within a quantum before its
/// priority is recomputed.
pub const USAGE_THRESHOLD: u32 = 1 << (PRIORITY_SHIFT + 2 + SCHEDULER_SHIFT);

/// Charges `ticks` ticks the current CPU spent in `state` to `thread`,
/// running on its processor: unless idle, its quantum runs down and is
/// renewed when it runs out, its priority follows its use, and the CPU
/// checks whether it should switch.
pub fn quantum_update(thread: &'static Thread, ticks: u32, state: CpuState) {
    let processor = smp::current().processor();
    // A processor moving between sets is in none.
    let Some(set) = processor.set() else {
        return;
    };
    let set_quantum = set.current_quantum();
    let mut quantum = if processor.bound_queued() == 0 {
        set_quantum
    } else {
        MIN_QUANTUM
    };
    if matches!(state, CpuState::Idle) {
        return;
    }
    let left = processor.quantum.load(Ordering::Relaxed) - i32::try_from(ticks).unwrap();
    processor.quantum.store(left, Ordering::Relaxed);
    if quantum != processor.last_quantum.load(Ordering::Relaxed)
        && set.processor_count.load(Ordering::Relaxed) > 1
    {
        processor.last_quantum.store(quantum, Ordering::Relaxed);
        quantum = set.adjust_quantum(quantum);
    }
    if left <= 0 {
        thread.charge_tick(true);
        processor.first_quantum.store(false, Ordering::Relaxed);
        let next = i32::try_from(thread.next_quantum(quantum)).unwrap();
        processor.quantum.store(left + next, Ordering::Relaxed);
    } else {
        thread.charge_tick(false);
    }
    ast::check(thread);
}

/// Whether a thread made runnable may preempt the current one.
#[derive(Clone, Copy)]
pub enum Preemption {
    /// It may, if it is better.
    Allowed,
    /// It may not.
    Forbidden,
}

/// Makes `thread` runnable: it goes on the run queue of the processor it
/// is bound to, which then switches if it is the current CPU's, or else on
/// the current CPU's, which switches if `preemption` allows it and `thread`
/// is better than the one it runs.
///
/// # Panics
///
/// If `thread` is bound to another running CPU, or is not bound and in
/// another set than the current CPU's: those come with SMP.
pub fn set_run(thread: &'static Thread, preemption: Preemption) {
    let _section = CriticalSection::enter();
    thread.catch_up();
    let cpu = smp::current();
    let processor = cpu.processor();
    if let Some(bound) = thread.bound() {
        bound.enqueue(thread);
        // Another CPU's is off line, so it takes no AST check.
        if ptr::eq(bound, processor) {
            cpu.add_asts(Ast::BLOCK);
        }
        return;
    }
    assert!(
        processor
            .set()
            .is_some_and(|set| ptr::eq(set, thread.set.load(Ordering::Acquire))),
        "thread_setrun: another set's run queue, not yet"
    );
    processor.enqueue(thread);
    if matches!(preemption, Preemption::Allowed) {
        let current = cpu
            .active_thread()
            .expect("thread_setrun: no current thread");
        if current.sched_pri.load(Ordering::Relaxed) > thread.sched_pri.load(Ordering::Relaxed) {
            processor.first_quantum.store(false, Ordering::Relaxed);
            cpu.add_asts(Ast::BLOCK);
        }
    }
}

/// Makes the current thread wait for `event`, uninterruptibly: it goes on
/// its CPU's sleep queue for the event, and stops running once it blocks.
///
/// # Panics
///
/// If it waits for an event already.
fn assert_wait(event: Event) {
    let _section = CriticalSection::enter();
    let cpu = smp::current();
    let thread = cpu.active_thread().expect("assert_wait: no current thread");
    let waited = thread.wait_event.load(Ordering::Relaxed);
    assert!(
        waited == 0,
        "assert_wait: already asserted event {waited:#x}"
    );
    // SAFETY: a thread lives while it waits on a sleep queue.
    cpu.sleep_queues()[event.hash()].push_back(unsafe { UnsafeRef::from_raw(thread) });
    thread.wait_event.store(event.0, Ordering::Relaxed);
    thread.state.fetch_or(
        (ThreadState::WAIT | ThreadState::UNINTERRUPTIBLE).bits(),
        Ordering::Relaxed,
    );
}

/// Makes the current thread wait for `event`, releases `guard`'s lock and
/// blocks; returns once the thread runs again.
pub fn sleep<T>(event: Event, guard: SpinlockGuard<'_, T>) {
    assert_wait(event);
    drop(guard);
    block();
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
    processor.first_quantum.store(true, Ordering::Relaxed);
    let set = processor.set().expect("thread_select: no processor set");
    let thread = match processor.dequeue() {
        Some(thread) if thread.bound().is_some() => {
            processor
                .quantum
                .store(i32::try_from(MIN_QUANTUM).unwrap(), Ordering::Relaxed);
            return thread;
        }
        Some(thread) => thread,
        None if current.state.load(Ordering::Relaxed) == ThreadState::RUN.bits()
            && ptr::eq(current.set.load(Ordering::Acquire), set)
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
            *processor
                .idle_thread
                .get()
                .expect("choose_pset_thread: no idle thread")
        }
    };
    let quantum = thread.next_quantum(set.quantum.load(Ordering::Relaxed));
    processor
        .quantum
        .store(i32::try_from(quantum).unwrap(), Ordering::Relaxed);
    thread
}

/// Makes `new` interruptible, then switches the current CPU to it from
/// `old`, the thread it runs, inside a critical section; returns once `old`
/// runs again, having dispatched the thread switched from then.
fn invoke(old: &'static Thread, new: &'static Thread) {
    new.state
        .fetch_and(!ThreadState::UNINTERRUPTIBLE.bits(), Ordering::Relaxed);
    if ptr::eq(old, new) {
        return;
    }
    new.run_here();
    // The CPU takes the new thread's ASTs; none is per-thread yet.
    smp::current().add_asts(Ast::from_bits_retain(new.asts.load(Ordering::Relaxed)));
    // SAFETY: the CPU runs on `old`'s stack; `new` was switched away from,
    // or got its stack before it was made runnable.
    dispatch(unsafe { old.switch(new) });
}

/// Puts `thread`, just switched from, where its state says: back on a run
/// queue if it runs on, nowhere if an idle thread; one that waits stops
/// running, unless suspended in an interruptible wait.
///
/// # Panics
///
/// If its state is not one of those.
pub fn dispatch(thread: &'static Thread) {
    match ThreadState::from_bits_retain(thread.state.load(Ordering::Relaxed)) {
        state if state == ThreadState::RUN => set_run(thread, Preemption::Forbidden),
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
            thread
                .state
                .fetch_and(!ThreadState::RUN.bits(), Ordering::Relaxed);
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
    let lowest = u32::try_from(RUN_QUEUES).unwrap() - 1;
    thread.priority.store(lowest, Ordering::Relaxed);
    thread.sched_pri.store(lowest, Ordering::Relaxed);
    thread
        .state
        .fetch_or(ThreadState::IDLE.bits(), Ordering::Relaxed);
    assert!(
        cpu.processor().idle_thread.set(thread).is_ok(),
        "idle_thread: twice"
    );
    drop(section);
    block();
    loop {
        // An interrupt that makes a thread runnable is held back until the
        // section ends, and wakes the CPU.
        let section = loop {
            let section = CriticalSection::enter();
            if smp::current().processor().queued.load(Ordering::Relaxed) != 0 {
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

/// Sets up the scheduler, on the boot CPU.
pub fn init() {
    processor::bootstrap();
}
