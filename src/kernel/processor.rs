// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Processors and processor sets.

use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::Error;
use super::sched::{MIN_QUANTUM, Policies, RUN_QUEUES, RunQueue, SCHEDULER_SCALE, SYSTEM_PRIORITY};
use super::slab::{Cache, CacheBox};
use super::task::{SetTaskAdapter, Task};
use super::thread::RunQueueAdapter;
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
    /// What it is doing.
    state: State,
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

/// What a processor is doing; only the states used here.
enum State {
    /// Not in the system.
    Offline,
}

impl Processor {
    /// One not yet in the system.
    pub const fn new() -> Self {
        Self {
            state: State::Offline,
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
    empty: bool,
    /// How many references it has.
    pub(super) references: AtomicU32,
    /// The highest priority its threads may take.
    pub(super) max_priority: u32,
    /// The policies its threads may use.
    policies: Policies,
    /// Its processors' quantum, in ticks.
    quantum: u32,
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
            empty: true,
            references: AtomicU32::new(1),
            max_priority: SYSTEM_PRIORITY,
            policies: Policies::TIMESHARE,
            quantum: MIN_QUANTUM,
            load: SCHEDULER_SCALE,
            port: AtomicPtr::new(ptr::null_mut()),
            name_port: AtomicPtr::new(ptr::null_mut()),
            tasks: [const { Token::new(LinkedList::new(SetTaskAdapter::NEW)) }; MEMBER_SHARDS],
            task_count: AtomicU32::new(0),
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
