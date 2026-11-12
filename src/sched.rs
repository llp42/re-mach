// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The scheduler.

use core::num::{NonZero, TryFromIntError};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicI64, AtomicU32, AtomicUsize, Ordering};

use intrusive_collections::{LinkedList, UnsafeRef};

use crate::arch::{self, Machine as _};
use crate::ast::{self, Ast};
use crate::clock;
use crate::clock::timeout::Timeout;
use crate::collections::ArrayVec;
use crate::config::{HZ, MAX_CPUS};
use crate::error::Error;
use crate::machine::CpuState;
use crate::processor::{self, Processor, State};
use crate::smp;
use crate::sync::{CriticalSection, OnceLock, SpinlockGuard};
use crate::thread::{Quantum, RunQueueAdapter, SleepQueueAdapter, Thread, ThreadState};

/// The threads sleeping on the events that hash to one slot.
pub type SleepQueue = LinkedList<SleepQueueAdapter>;

/// How many sleep queues each CPU has.
pub const SLEEP_QUEUES: usize = 1031;

/// What a thread waits for: an address, never 0.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Event(NonZero<usize>);

impl Event {
    /// The event of `object`'s address.
    pub fn of<T>(object: &T) -> Self {
        Self(NonNull::from(object).addr())
    }

    /// Its sleep queue: its address as a signed number, complemented if
    /// negative, modulo the queues.
    const fn hash(self) -> usize {
        let address = self.0.get().cast_signed();
        let positive = if address < 0 { !address } else { address };
        positive.cast_unsigned() % SLEEP_QUEUES
    }
}

impl From<Event> for usize {
    fn from(event: Event) -> Self {
        event.0.get()
    }
}

/// Fails for 0, which no event is.
impl TryFrom<usize> for Event {
    type Error = TryFromIntError;

    fn try_from(address: usize) -> Result<Self, TryFromIntError> {
        NonZero::try_from(address).map(Self)
    }
}

/// How a thread's wait ended, as Mach numbers it; only the results used
/// here.
pub enum WaitResult {
    /// It was woken.
    Awakened,
    /// Its timer ran out.
    TimedOut,
}

impl WaitResult {
    /// Its number.
    pub(super) const fn number(self) -> u8 {
        match self {
            Self::Awakened => 0,
            Self::TimedOut => 1,
        }
    }
}

/// The runnable threads of one priority.
pub type RunQueue = LinkedList<RunQueueAdapter>;

/// How many run queues each processor has, one per priority.
pub const RUN_QUEUES: usize = 65;

/// A thread's, task's or set's priority: Mach counts 0 as the highest.
#[derive(Clone, Copy)]
pub struct Priority(u32);

impl Priority {
    /// The highest, the first run queue's.
    pub const HIGHEST: Self = Self(0);

    /// The lowest, the last run queue's.
    pub const LOWEST: Self = Self(64);

    /// Whether it is higher than `other`.
    pub const fn outranks(self, other: Self) -> bool {
        self.0 < other.0
    }

    /// The lower of it and `other`.
    pub fn weaker(self, other: Self) -> Self {
        Self(self.0.max(other.0))
    }

    /// Its run queue's index.
    pub fn index(self) -> usize {
        usize::try_from(self.0).unwrap()
    }

    /// The priority a thread of this base priority is scheduled at after
    /// `usage`, within the run queues.
    pub fn aged_by(self, usage: u32) -> Self {
        Self((self.0 + (usage >> (PRIORITY_SHIFT + SCHEDULER_SHIFT))).min(Self::LOWEST.0))
    }
}

impl From<Priority> for u32 {
    fn from(priority: Priority) -> Self {
        priority.0
    }
}

/// Fails with `InvalidArgument` past the lowest.
impl TryFrom<u32> for Priority {
    type Error = Error;

    fn try_from(priority: u32) -> Result<Self, Error> {
        if priority <= Self::LOWEST.0 {
            Ok(Self(priority))
        } else {
            Err(Error::InvalidArgument)
        }
    }
}

/// The processors waiting for the action thread to shut them down or move
/// them to another set, a bit for each CPU: set with Release, taken with
/// Acquire.
static ACTION: AtomicUsize = AtomicUsize::new(0);

/// How many times threads' priorities have been recomputed, once a second.
static SCHEDULER_TICK: AtomicU32 = AtomicU32::new(0);

/// The scheduler tick.
pub fn scheduler_tick() -> u32 {
    SCHEDULER_TICK.load(Ordering::Relaxed)
}

/// The scheduler thread, set as it starts.
static SCHED_THREAD: OnceLock<&'static Thread> = OnceLock::new();

/// What recomputes threads' priorities each second.
pub static RECOMPUTE_PRIORITIES: Timeout = Timeout::new(recompute_priorities);

/// Ages threads' use once more and sets itself again for a second later,
/// on the boot CPU, then once a second; it wakes the scheduler thread,
/// once that has started.
pub fn recompute_priorities(_: &'static Timeout) {
    SCHEDULER_TICK.fetch_add(1, Ordering::Relaxed);
    clock::timeout::set_timeout(&RECOMPUTE_PRIORITIES, u64::from(HZ));
    if let Some(&thread) = SCHED_THREAD.get() {
        // Its home CPU is this one while only the boot CPU runs threads.
        clear_wait(thread, WaitResult::Awakened);
    }
}

/// What Mach factors and load averages are scaled by.
const LOAD_SCALE: i64 = 1000;

/// How much of its last value each system average keeps, over
/// `LOAD_SCALE`: they span 5 seconds, 30 seconds and a minute.
const DECAY: [i64; 3] = [800, 966, 983];

/// The default set's load averages over those spans, by `LOAD_SCALE`;
/// written only by `compute_mach_factor`.
static AVENRUN: [AtomicI64; 3] = [const { AtomicI64::new(0) }; 3];

/// The default set's Mach factors over those spans, by `LOAD_SCALE`;
/// written only by `compute_mach_factor`.
static MACH_FACTOR: [AtomicI64; 3] = [const { AtomicI64::new(0) }; 3];

/// The default set's load averages, by `LOAD_SCALE`.
pub fn load_averages() -> [i64; 3] {
    AVENRUN
        .each_ref()
        .map(|average| average.load(Ordering::Relaxed))
}

/// The default set's Mach factors, by `LOAD_SCALE`.
pub fn mach_factors() -> [i64; 3] {
    MACH_FACTOR
        .each_ref()
        .map(|factor| factor.load(Ordering::Relaxed))
}

/// Computes each processor set's Mach factor and loads, under the sets'
/// token and its own, from the threads its processors queue and run, the
/// current one aside; the default set's feed the system averages. A set
/// with no processor has none.
pub fn compute_mach_factor() {
    let all = processor::ALL_SETS.lock();
    for set in &all.list {
        let members = set.lock();
        let count = i64::from(set.processor_count());
        if count > 0 {
            // Each processor's run queues hold the set's threads too.
            let (queued, idle) = (0..MAX_CPUS)
                .map(|cpu| smp::of(cpu).processor())
                .filter(|processor| processor.set().is_some_and(|its| ptr::eq(its, set)))
                .fold((0, 0), |(queued, idle), processor| {
                    (
                        queued + i64::from(processor.queued()),
                        idle + i64::from(matches!(processor.state(), State::Idle)),
                    )
                });
            let is_default = ptr::eq(set, &raw const processor::DEFAULT_SET);
            // Less the current thread, in the default set.
            let threads = queued + count - idle - i64::from(is_default);
            let (factor, load) = if threads > count {
                (
                    count * LOAD_SCALE / (threads + 1),
                    (threads << SCHEDULER_SHIFT) / count,
                )
            } else {
                ((count - threads) * LOAD_SCALE, i64::from(SCHEDULER_SCALE))
            };
            let average = threads * LOAD_SCALE;
            set.update_averages(factor, average, u32::try_from(load).unwrap());
            if is_default {
                for (decay, (mach_factor, avenrun)) in
                    DECAY.into_iter().zip(MACH_FACTOR.iter().zip(&AVENRUN))
                {
                    let decayed = |old: i64, now: i64| {
                        (old * decay + now * (LOAD_SCALE - decay)) / LOAD_SCALE
                    };
                    mach_factor.store(
                        decayed(mach_factor.load(Ordering::Relaxed), factor),
                        Ordering::Relaxed,
                    );
                    avenrun.store(
                        decayed(avenrun.load(Ordering::Relaxed), average),
                        Ordering::Relaxed,
                    );
                }
            }
        }
        drop(members);
    }
    drop(all);
}

/// The priority of system threads, and the default set's highest.
pub const SYSTEM_PRIORITY: Priority = Priority(6);

/// The priority of user threads.
pub const USER_PRIORITY: Priority = Priority(25);

/// The shortest quantum, in ticks: 33 context switches a second.
pub const MIN_QUANTUM: u32 = HZ / 33;

/// The scheduler's fixed-point one, which a set's load is scaled by.
pub const SCHEDULER_SCALE: u32 = 128;

/// How far `SCHEDULER_SCALE` shifts.
const SCHEDULER_SHIFT: u32 = 7;

/// How far a thread's use is shifted, past `SCHEDULER_SHIFT`, to age its
/// priority.
const PRIORITY_SHIFT: u32 = 17;

/// A thread's scheduling policy, one of `Policies`.
#[derive(Clone, Copy)]
pub enum Policy {
    /// Its priority decays with use.
    Timeshare,
    /// Its priority stays, and it has its own quantum.
    #[expect(dead_code, reason = "thread_policy sets it")]
    FixedPriority,
}

bitflags::bitflags! {
    /// The scheduling policies a processor set allows, as Mach's interface
    /// numbers them; only those used here.
    #[derive(Clone, Copy)]
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
    let left = processor.charge_quantum(ticks);
    if quantum != processor.last_quantum() && set.processor_count() > 1 {
        processor.set_last_quantum(quantum);
        quantum = set.adjust_quantum(quantum);
    }
    if left <= 0 {
        thread.charge_tick(Quantum::Expired);
        processor.end_first_quantum();
        let next = i32::try_from(thread.next_quantum(quantum)).unwrap();
        processor.set_quantum(left + next);
    } else {
        thread.charge_tick(Quantum::Left);
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
pub fn make_runnable(thread: &'static Thread, preemption: Preemption) {
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
        processor.set().is_some_and(|set| thread.is_in(set)),
        "thread_setrun: another set's run queue, not yet"
    );
    processor.enqueue(thread);
    if matches!(preemption, Preemption::Allowed) {
        let current = cpu
            .active_thread()
            .expect("thread_setrun: no current thread");
        if thread
            .scheduled_priority()
            .outranks(current.scheduled_priority())
        {
            processor.end_first_quantum();
            cpu.add_asts(Ast::BLOCK);
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
        // SAFETY: a thread lives while it waits on a sleep queue.
        cpu.sleep_queues()[event.hash()].push_back(unsafe { UnsafeRef::from_raw(thread) });
        thread.set_wait_event(Some(event));
    }
    thread.insert_state(ThreadState::WAIT | ThreadState::UNINTERRUPTIBLE);
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
        // SAFETY: it waited on the queue, and goes on a run queue.
        let thread = unsafe { &*UnsafeRef::into_raw(cursor.remove().unwrap()) };
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
/// CPU: it leaves the sleep queue of its event and runs; a thread not
/// waiting is left as it is.
///
/// # Panics
///
/// If it runs already, or is suspended in an interruptible wait: not yet.
pub fn clear_wait(thread: &'static Thread, result: WaitResult) {
    let _section = CriticalSection::enter();
    if let Some(event) = thread.wait_event() {
        let mut queues = smp::current().sleep_queues();
        // SAFETY: a thread waiting for an event is on its home CPU's queue
        // for it.
        unsafe { queues[event.hash()].cursor_mut_from_ptr(thread) }.remove();
        drop(queues);
        thread.set_wait_event(None);
    }
    clock::timeout::reset_timeout_check(thread.timer());
    let state = thread.state();
    if !state.contains(ThreadState::WAIT) {
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
    // The CPU takes the new thread's ASTs; none is per-thread yet.
    smp::current().add_asts(new.asts());
    // SAFETY: the CPU runs on `old`'s stack; `new` was switched away from,
    // or got its stack before it was made runnable.
    dispatch(unsafe { old.switch(new) });
}

/// Puts `thread`, just switched from, where its state says: back on a run
/// queue if it runs on, nowhere if an idle thread; one suspended outside a
/// wait stops, and one that waits stops running, unless suspended in an
/// interruptible wait.
///
/// # Panics
///
/// If its state is not one of those.
pub fn dispatch(thread: &'static Thread) {
    match thread.state() {
        state if state == ThreadState::RUN => make_runnable(thread, Preemption::Forbidden),
        state if state == ThreadState::RUN | ThreadState::SUSPENDED => {
            thread.remove_state(ThreadState::RUN);
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

/// The scheduler thread's work: it records itself as the scheduler
/// thread, then waits with no event, uninterruptibly, for
/// `recompute_priorities` to wake it; woken, it computes the Mach factor
/// and, at an odd scheduler tick, scans for stuck threads, then waits
/// again.
///
/// # Panics
///
/// If a scheduler thread ran already.
pub fn sched_thread() -> ! {
    let thread = smp::current()
        .active_thread()
        .expect("sched_thread: no current thread");
    assert!(SCHED_THREAD.set(thread).is_ok(), "sched_thread: twice");
    loop {
        assert_wait(None);
        block();
        compute_mach_factor();
        if scheduler_tick() % 2 == 1 {
            do_thread_scan();
        }
    }
}

/// How many stuck threads a scan takes off the run queues at a time.
pub const MAX_STUCK_THREADS: usize = 16;

/// Catches up the stuck threads on the current CPU's run queues, each CPU
/// scanning its own: a pass takes up to `MAX_STUCK_THREADS` off the queues,
/// then makes runnable again each whose scheduling state is still only
/// `RUN`; passes go on while one finds no room.
fn do_thread_scan() {
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

/// The action thread's work: each time it runs it waits on `ACTION`,
/// uninterruptibly, then takes the processors there, so far none, and
/// blocks.
///
/// # Panics
///
/// If a processor waits there: not yet.
pub fn action_thread() -> ! {
    loop {
        // Before the check, so a later wake is not lost.
        assert_wait(Some(Event::of(&ACTION)));
        assert!(
            ACTION.swap(0, Ordering::Acquire) == 0,
            "processor_doaction: not yet"
        );
        block();
    }
}

/// Sets up the scheduler, on the boot CPU.
pub fn init() {
    processor::bootstrap();
}
