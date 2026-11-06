// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Processors and processor sets.

use core::ops::DerefMut;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU32, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::config::MAX_CPUS;
use crate::error::Error;
use crate::ipc::port::Port;
use crate::sched::{MIN_QUANTUM, Policies, RUN_QUEUES, RunQueue, SCHEDULER_SCALE, SYSTEM_PRIORITY};
use crate::slab::{Cache, CacheBox};
use crate::smp;
use crate::sync::{CpuLock, CriticalSection, OnceLock, Token};
use crate::task::{SetTaskAdapter, Task};
use crate::thread::{RunQueueAdapter, SetThreadAdapter, Thread};
use crate::{ipc_host, machine};

/// A processor: one per CPU, in that CPU's data.
#[expect(dead_code, reason = "the dispatcher and processor_info read them")]
pub struct Processor {
    /// What it is doing, a `State`.
    state: AtomicU8,
    /// How many threads wait on its run queues.
    pub(super) queued: AtomicU32,
    /// The ticks left of its thread's quantum.
    pub(super) quantum: AtomicI32,
    /// Whether its thread runs its first quantum in succession.
    pub(super) first_quantum: AtomicBool,
    /// The quantum it last gave out, before adjustment.
    pub(super) last_quantum: AtomicU32,
    /// The processor set it is in, once it comes up.
    set: AtomicPtr<ProcessorSet>,
    /// Its runnable threads.
    run_queues: CpuLock<RunQueues>,
    /// Its idle thread, once that runs.
    pub(super) idle_thread: OnceLock<&'static Thread>,
    /// Its place in the action queue.
    action_link: LinkedListAtomicLink,
    /// Its port, for controlling it.
    pub(super) port: AtomicPtr<Port>,
    /// Its name port, for information about it.
    pub(super) name_port: AtomicPtr<Port>,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub ActionQueueAdapter = &'static Processor: Processor { action_link => LinkedListAtomicLink }
);

/// What a processor is doing, as Mach numbers it; only the states used
/// here.
pub enum State {
    /// Not in the system.
    Offline = 0,
    /// Running a thread.
    Running = 1,
    /// Running its idle thread.
    Idle = 2,
}

impl State {
    /// Its number.
    pub(super) const fn number(self) -> u8 {
        match self {
            Self::Offline => 0,
            Self::Running => 1,
            Self::Idle => 2,
        }
    }
}

/// A processor's runnable threads, bound to it or not.
struct RunQueues {
    /// By priority, each in the order they became runnable.
    lists: [RunQueue; RUN_QUEUES],
    /// How many of them are bound to it.
    bound: u32,
}

impl Processor {
    /// One not yet in the system.
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(State::Offline.number()),
            queued: AtomicU32::new(0),
            quantum: AtomicI32::new(0),
            first_quantum: AtomicBool::new(false),
            last_quantum: AtomicU32::new(0),
            set: AtomicPtr::new(ptr::null_mut()),
            // SAFETY: only `run_queues` locks it, once it checked the CPU.
            run_queues: unsafe {
                CpuLock::new(RunQueues {
                    lists: [const { RunQueue::new(RunQueueAdapter::NEW) }; RUN_QUEUES],
                    bound: 0,
                })
            },
            idle_thread: OnceLock::new(),
            action_link: LinkedListAtomicLink::new(),
            port: AtomicPtr::new(ptr::null_mut()),
            name_port: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// What it is doing.
    ///
    /// # Panics
    ///
    /// If its state is not one used here.
    pub(super) fn state(&self) -> State {
        match self.state.load(Ordering::Acquire) {
            0 => State::Offline,
            1 => State::Running,
            2 => State::Idle,
            state => panic!("processor: bad state {state}"),
        }
    }

    /// Sets what it is doing, on its CPU.
    pub(super) fn set_state(&self, state: State) {
        self.state.store(state.number(), Ordering::Release);
    }

    /// Its run queues, locked on its CPU, or on another before it starts.
    ///
    /// # Panics
    ///
    /// On another CPU once it runs: that comes with SMP.
    fn run_queues(&self) -> impl DerefMut<Target = RunQueues> + '_ {
        // The thread stays on the CPU checked until the lock enters its own
        // section.
        let _section = CriticalSection::enter();
        assert!(
            ptr::eq(self, smp::current().processor()) || matches!(self.state(), State::Offline),
            "run queue: another running CPU's, not yet"
        );
        self.run_queues.lock()
    }

    /// How many threads bound to it wait on its run queues, on its CPU.
    pub(super) fn bound_queued(&self) -> u32 {
        self.run_queues().bound
    }

    /// Puts `thread` last at its scheduled priority.
    pub(super) fn enqueue(&self, thread: &'static Thread) {
        let mut queues = self.run_queues();
        let priority = usize::try_from(thread.sched_pri.load(Ordering::Relaxed)).unwrap();
        // SAFETY: a thread lives while it waits on a run queue.
        queues.lists[priority].push_back(unsafe { UnsafeRef::from_raw(thread) });
        if thread.bound().is_some() {
            queues.bound += 1;
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
    }

    /// Takes the best of its threads bound to it, or if none the best of
    /// all; the first to become runnable among equals.
    ///
    /// # Panics
    ///
    /// If it counts bound threads but has none.
    pub(super) fn dequeue(&self) -> Option<&'static Thread> {
        let mut queues = self.run_queues();
        let bound_only = queues.bound > 0;
        let Some(thread) = queues.lists.iter_mut().find_map(|list| {
            let mut cursor = list.front_mut();
            while let Some(thread) = cursor.get() {
                if !bound_only || thread.bound().is_some() {
                    return cursor.remove();
                }
                cursor.move_next();
            }
            None
        }) else {
            assert!(!bound_only, "choose_thread: no bound thread");
            return None;
        };
        // SAFETY: as above.
        let thread = unsafe { &*UnsafeRef::into_raw(thread) };
        if bound_only {
            queues.bound -= 1;
        }
        self.queued.fetch_sub(1, Ordering::Relaxed);
        Some(thread)
    }

    /// The set it is in, once it comes up.
    pub(super) fn set(&self) -> Option<&'static ProcessorSet> {
        // SAFETY: a set lives while it has processors.
        unsafe { self.set.load(Ordering::Acquire).as_ref() }
    }
}

/// A set of processors, and the threads that run on them.
#[expect(dead_code, reason = "processor_set_info and thread_create read them")]
pub struct ProcessorSet {
    /// Whether it is in use; a destroyed set is not.
    pub(super) active: bool,
    /// Whether it has no processor; threads created in it start suspended.
    /// Changed only under `members`' token.
    pub(super) empty: AtomicBool,
    /// How many processors it has; changed only under `members`' token, and
    /// read without it at each tick.
    pub(super) processor_count: AtomicU32,
    /// Which of its processors the next quantum adjustment falls to.
    adjust_index: AtomicU32,
    /// How many references it has.
    pub(super) references: AtomicU32,
    /// The highest priority its threads may take.
    pub(super) max_priority: u32,
    /// The policies its threads may use.
    pub(super) policies: Policies,
    /// Its processors' quantum, in ticks.
    pub(super) quantum: AtomicU32,
    /// The quantum for each count of threads waiting, up to its processor
    /// count; at 0, twice the quantum of 1. Changed only under `members`'
    /// token, and read without it at each tick.
    machine_quantum: [AtomicU32; MAX_CPUS + 1],
    /// Its load, scaled by `SCHEDULER_SCALE`.
    pub(super) load: u32,
    /// Its port, for controlling it.
    pub(super) port: AtomicPtr<Port>,
    /// Its name port, for information about it.
    pub(super) name_port: AtomicPtr<Port>,
    /// Its tasks and threads, and how many of each.
    pub(super) members: Token<Members>,
    /// Its place in the list of all sets.
    all_link: LinkedListAtomicLink,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    AllSetsAdapter = UnsafeRef<ProcessorSet>: ProcessorSet { all_link => LinkedListAtomicLink }
);

/// A set's tasks and threads, and how many of each.
pub struct Members {
    /// The tasks, in the order the set took them.
    tasks: LinkedList<SetTaskAdapter>,
    /// How many tasks there are.
    task_count: u32,
    /// The threads, in the order the set took them.
    threads: LinkedList<SetThreadAdapter>,
    /// How many threads there are.
    thread_count: u32,
}

/// Every processor set, and how many there are.
struct AllSets {
    list: LinkedList<AllSetsAdapter>,
    count: u32,
}

/// The processor sets, behind one token: they change only as sets are
/// created and destroyed.
static ALL_SETS: Token<AllSets> = Token::new(AllSets {
    list: LinkedList::new(AllSetsAdapter::NEW),
    count: 0,
});

/// The set the kernel creates at boot, where the other CPUs go.
pub static SLAVE_SET: OnceLock<&'static ProcessorSet> = OnceLock::new();

impl ProcessorSet {
    /// A new set, not in use yet, with one reference, no processor and no
    /// task.
    const fn new() -> Self {
        Self {
            active: false,
            empty: AtomicBool::new(true),
            processor_count: AtomicU32::new(0),
            adjust_index: AtomicU32::new(0),
            references: AtomicU32::new(1),
            max_priority: SYSTEM_PRIORITY,
            policies: Policies::TIMESHARE,
            quantum: AtomicU32::new(MIN_QUANTUM),
            machine_quantum: [const { AtomicU32::new(MIN_QUANTUM) }; MAX_CPUS + 1],
            load: SCHEDULER_SCALE,
            port: AtomicPtr::new(ptr::null_mut()),
            name_port: AtomicPtr::new(ptr::null_mut()),
            members: Token::new(Members {
                tasks: LinkedList::new(SetTaskAdapter::NEW),
                task_count: 0,
                threads: LinkedList::new(SetThreadAdapter::NEW),
                thread_count: 0,
            }),
            all_link: LinkedListAtomicLink::new(),
        }
    }

    /// A new set, in use, in the list of all sets, with its ports and
    /// references for them, and for the caller's set and name.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no set can be allocated.
    pub fn create() -> Result<&'static Self, Error> {
        let mut set = SET_CACHE.allocate(Self::new())?;
        // The caller's set and name.
        set.references.fetch_add(2, Ordering::Relaxed);
        set.active = true;
        // SAFETY: a set lives until it is destroyed, which comes later.
        let set = unsafe { CacheBox::into_raw(set).as_ref() };
        ipc_host::init_set(set);
        set.join_all();
        Ok(set)
    }

    /// Takes `thread`, new and in no set, into its `members`; the thread
    /// keeps the reference the caller took for it.
    pub(super) fn add_thread(&'static self, members: &mut Members, thread: &'static Thread) {
        // SAFETY: threads are never freed while a set holds them.
        members
            .threads
            .push_back(unsafe { UnsafeRef::from_raw(thread) });
        thread
            .set
            .store(ptr::from_ref(self).cast_mut(), Ordering::Release);
        members.thread_count += 1;
    }

    /// Takes `processor`, coming up, and sets its quanta again; the caller
    /// holds `members`' token.
    pub(super) fn add_processor(&'static self, processor: &'static Processor) {
        processor
            .set
            .store(ptr::from_ref(self).cast_mut(), Ordering::Release);
        let count = self.processor_count.fetch_add(1, Ordering::Relaxed) + 1;
        self.empty.store(false, Ordering::Relaxed);
        self.set_quantum(count);
    }

    /// Sets its quanta for `count` processors: an even share of the
    /// shortest quantum each, by threads waiting; with no set-wide run
    /// queue, the quantum for none waiting.
    fn set_quantum(&self, count: u32) {
        for waiting in 1..=count {
            self.machine_quantum[usize::try_from(waiting).unwrap()].store(
                (MIN_QUANTUM * count + waiting / 2) / waiting,
                Ordering::Relaxed,
            );
        }
        let one = self.machine_quantum[1].load(Ordering::Relaxed);
        self.machine_quantum[0].store(2 * one, Ordering::Relaxed);
        self.quantum.store(2 * one, Ordering::Relaxed);
    }

    /// Its quantum for the threads waiting now: with no set-wide run queue,
    /// for none.
    pub(super) fn current_quantum(&self) -> u32 {
        let quantum = self.machine_quantum[0].load(Ordering::Relaxed);
        self.quantum.store(quantum, Ordering::Relaxed);
        quantum
    }

    /// `quantum` adjusted at run time: its processors take turns at
    /// quanta spread from the shortest to it, so theirs do not run out
    /// together. It has more than one processor.
    pub(super) fn adjust_quantum(&self, quantum: u32) -> u32 {
        let count = self.processor_count.load(Ordering::Relaxed);
        let index = self
            .adjust_index
            .update(Ordering::Relaxed, Ordering::Relaxed, |index| {
                if index + 1 >= count { 0 } else { index + 1 }
            });
        MIN_QUANTUM + index * (quantum - MIN_QUANTUM) / (count - 1)
    }

    /// Adds it to the list of all sets.
    fn join_all(&'static self) {
        let mut all = ALL_SETS.lock();
        // SAFETY: a set leaves the list before it is freed.
        all.list.push_back(unsafe { UnsafeRef::from_raw(self) });
        all.count += 1;
        drop(all);
    }

    /// Takes `task`, new and in no set, with a reference for it.
    pub(super) fn add_task(&'static self, task: &'static Task) {
        self.references.fetch_add(1, Ordering::Relaxed);
        let mut members = self.members.lock();
        // SAFETY: tasks are never freed while a set holds them.
        members
            .tasks
            .push_back(unsafe { UnsafeRef::from_raw(task) });
        task.set
            .store(ptr::from_ref(self).cast_mut(), Ordering::Release);
        members.task_count += 1;
        drop(members);
    }
}

/// The default set, where every processor and thread starts.
pub static DEFAULT_SET: ProcessorSet = {
    let mut set = ProcessorSet::new();
    set.active = true;
    set
};

/// The cache processor sets other than the default come from.
static SET_CACHE: Cache<ProcessorSet> = Cache::new("processor_set");

/// The boot CPU's processor.
pub static BOOT_PROCESSOR: OnceLock<&'static Processor> = OnceLock::new();

/// Records the boot CPU's processor, on the boot CPU.
///
/// # Panics
///
/// On a second call.
pub fn bootstrap() {
    assert!(
        BOOT_PROCESSOR.set(smp::current().processor()).is_ok(),
        "processor: bootstrapped twice"
    );
}

/// Sets up processor sets, on the boot CPU: gives every processor but the
/// boot CPU's its ports, and creates the slave set.
pub fn init() {
    SET_CACHE.register();
    for cpu in (1..MAX_CPUS).filter(|&cpu| machine::has_processor(cpu)) {
        ipc_host::init_processor(smp::of(cpu).processor());
    }
    DEFAULT_SET.join_all();
    let set = ProcessorSet::create().unwrap_or_else(|error| panic!("pset_sys_init: {error:?}"));
    assert!(SLAVE_SET.set(set).is_ok(), "pset_sys_init: twice");
}

/// Brings the current CPU's processor up, on that CPU, under the default
/// set's token and the slave set's: it joins the default set, or the slave
/// set for a CPU but the boot CPU's, and runs.
pub fn up(cpu: usize) {
    let processor = smp::current().processor();
    let slave_set = SLAVE_SET.get().expect("cpu_up: no slave set");
    let default_members = DEFAULT_SET.members.lock();
    let slave_members = slave_set.members.lock();
    let set = if cpu == 0 { &DEFAULT_SET } else { slave_set };
    set.add_processor(processor);
    processor
        .state
        .store(State::Running.number(), Ordering::Release);
    drop(slave_members);
    drop(default_members);
}
