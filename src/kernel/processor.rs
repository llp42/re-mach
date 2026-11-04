// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Processors and processor sets.

use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU8, AtomicU32, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::Error;
use super::sched::{MIN_QUANTUM, Policies, RUN_QUEUES, RunQueue, SCHEDULER_SCALE, SYSTEM_PRIORITY};
use super::slab::{Cache, CacheBox};
use super::task::{SetTaskAdapter, Task};
use super::thread::{RunQueueAdapter, SetThreadAdapter, Thread};
use super::{ipc_host, machine};
use crate::config::MAX_CPUS;
use crate::ipc::port::Port;
use crate::smp;
use crate::sync::{OnceLock, Token};

/// How many lists a set's tasks are spread over by address, each behind
/// its own token, so no lock covers all of them.
const MEMBER_SHARDS: usize = 64;

/// A processor: one per CPU, in that CPU's data.
#[expect(dead_code, reason = "the dispatcher and processor_info read them")]
pub struct Processor {
    /// What it is doing, a `State`.
    state: AtomicU8,
    /// The processor set it is in, once it comes up.
    set: AtomicPtr<ProcessorSet>,
    /// Its runnable threads, by priority; only its CPU touches them, inside
    /// a critical section.
    run_queues: [RunQueue; RUN_QUEUES],
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
enum State {
    /// Not in the system.
    Offline = 0,
    /// Running a thread.
    Running = 1,
}

impl State {
    /// Its number.
    const fn number(self) -> u8 {
        match self {
            Self::Offline => 0,
            Self::Running => 1,
        }
    }
}

impl Processor {
    /// One not yet in the system.
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(State::Offline.number()),
            set: AtomicPtr::new(ptr::null_mut()),
            run_queues: [const { RunQueue::new(RunQueueAdapter::NEW) }; RUN_QUEUES],
            action_link: LinkedListAtomicLink::new(),
            port: AtomicPtr::new(ptr::null_mut()),
            name_port: AtomicPtr::new(ptr::null_mut()),
        }
    }
}

/// A set of processors, and the threads that run on them.
#[expect(dead_code, reason = "processor_set_info and thread_create read them")]
pub struct ProcessorSet {
    /// Whether it is in use; a destroyed set is not.
    pub(super) active: bool,
    /// Whether it has no processor; threads created in it start suspended.
    pub(super) empty: AtomicBool,
    /// How many processors it has.
    processor_count: AtomicU32,
    /// How many references it has.
    pub(super) references: AtomicU32,
    /// The highest priority its threads may take.
    pub(super) max_priority: u32,
    /// The policies its threads may use.
    policies: Policies,
    /// Its processors' quantum, in ticks.
    quantum: AtomicU32,
    /// The quantum for each count of threads waiting, up to its processor
    /// count; at 0, twice the quantum of 1.
    machine_quantum: [AtomicU32; MAX_CPUS + 1],
    /// Its load, scaled by `SCHEDULER_SCALE`.
    load: u32,
    /// Its port, for controlling it.
    pub(super) port: AtomicPtr<Port>,
    /// Its name port, for information about it.
    pub(super) name_port: AtomicPtr<Port>,
    /// Its tasks, by address.
    tasks: [Token<LinkedList<SetTaskAdapter>>; MEMBER_SHARDS],
    /// How many tasks it has.
    task_count: AtomicU32,
    /// Its threads, by address.
    threads: [Token<LinkedList<SetThreadAdapter>>; MEMBER_SHARDS],
    /// How many threads it has.
    thread_count: AtomicU32,
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
pub(super) static SLAVE_SET: OnceLock<&'static ProcessorSet> = OnceLock::new();

impl ProcessorSet {
    /// A new set, not in use yet, with one reference, no processor and no
    /// task.
    const fn new() -> Self {
        Self {
            active: false,
            empty: AtomicBool::new(true),
            processor_count: AtomicU32::new(0),
            references: AtomicU32::new(1),
            max_priority: SYSTEM_PRIORITY,
            policies: Policies::TIMESHARE,
            quantum: AtomicU32::new(MIN_QUANTUM),
            machine_quantum: [const { AtomicU32::new(MIN_QUANTUM) }; MAX_CPUS + 1],
            load: SCHEDULER_SCALE,
            port: AtomicPtr::new(ptr::null_mut()),
            name_port: AtomicPtr::new(ptr::null_mut()),
            tasks: [const { Token::new(LinkedList::new(SetTaskAdapter::NEW)) }; MEMBER_SHARDS],
            task_count: AtomicU32::new(0),
            threads: [const { Token::new(LinkedList::new(SetThreadAdapter::NEW)) }; MEMBER_SHARDS],
            thread_count: AtomicU32::new(0),
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
        set.join_all();
        ipc_host::init_set(set);
        Ok(set)
    }

    /// Takes `thread`, new and in no set, with a reference for it.
    pub(super) fn add_thread(&'static self, thread: &'static Thread) {
        self.references.fetch_add(1, Ordering::Relaxed);
        let shard = &self.threads[ptr::from_ref(thread).addr() / 64 % MEMBER_SHARDS];
        // SAFETY: threads are never freed while a set holds them.
        shard
            .lock()
            .push_back(unsafe { UnsafeRef::from_raw(thread) });
        thread
            .set
            .store(ptr::from_ref(self).cast_mut(), Ordering::Release);
        self.thread_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Takes `processor`, coming up, and sets its quanta again.
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
        let shard = &self.tasks[ptr::from_ref(task).addr() / 64 % MEMBER_SHARDS];
        // SAFETY: tasks are never freed while a set holds them.
        shard.lock().push_back(unsafe { UnsafeRef::from_raw(task) });
        task.set
            .store(ptr::from_ref(self).cast_mut(), Ordering::Release);
        self.task_count.fetch_add(1, Ordering::Relaxed);
    }
}

/// The default set, where every processor and thread starts.
pub(super) static DEFAULT_SET: ProcessorSet = {
    let mut set = ProcessorSet::new();
    set.active = true;
    set
};

/// The cache processor sets other than the default come from.
static SET_CACHE: Cache<ProcessorSet> = Cache::new("processor_set");

/// The boot CPU's processor.
pub(super) static BOOT_PROCESSOR: OnceLock<&'static Processor> = OnceLock::new();

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

/// Brings the current CPU's processor up, on that CPU: it joins the
/// default set, or the slave set for a CPU but the boot CPU's, and runs.
pub fn up(cpu: usize) {
    let processor = smp::current().processor();
    let set = if cpu == 0 {
        &DEFAULT_SET
    } else {
        SLAVE_SET.get().expect("cpu_up: no slave set")
    };
    set.add_processor(processor);
    processor
        .state
        .store(State::Running.number(), Ordering::Release);
}
