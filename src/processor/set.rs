// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::ManuallyDrop;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};

use crate::config::MAX_CPUS;
use crate::error::Error;
use crate::ipc::port::port::Port;
use crate::ipc_host;
use crate::reference::{Count, Counted, Reference};
use crate::sched::policy::{MIN_QUANTUM, Policies, SCHEDULER_SCALE};
use crate::sched::priority::{Priority, SYSTEM_PRIORITY};
use crate::slab::CacheBox;
use crate::smp;
use crate::sync::{Token, TokenGuard};
use crate::task::task::{SetTaskAdapter, Task};
use crate::thread::thread::{SetThreadAdapter, Thread};

use super::all_sets::{ALL_SETS, DEFAULT_SET, SET_CACHE};
use super::members::Members;
use super::processor::Processor;

/// A set of processors, and the threads that run on them.
#[expect(dead_code, reason = "processor_set_info and thread_create read them")]
pub struct ProcessorSet {
    /// Whether it is in use; a destroyed set is not. Changed only under
    /// `members`' token.
    pub(super) active: AtomicBool,
    /// Whether it has no processor; threads created in it start suspended.
    /// Changed only under `members`' token.
    empty: AtomicBool,
    /// How many processors it has; changed only under `members`' token, and
    /// read without it at each tick.
    processor_count: AtomicU32,
    /// Which of its processors the next quantum adjustment falls to.
    adjust_index: AtomicU32,
    /// How many references it has.
    references: Count,
    /// The highest priority its threads may take.
    max_priority: Priority,
    /// The policies its threads may use.
    policies: Policies,
    /// Its processors' quantum, in ticks.
    quantum: AtomicU32,
    /// The quantum for each count of threads waiting, up to its processor
    /// count; at 0, twice the quantum of 1. Changed only under `members`'
    /// token, and read without it at each tick.
    machine_quantum: [AtomicU32; MAX_CPUS + 1],
    /// Its Mach factor, scaled by `LOAD_SCALE`; written only by
    /// `compute_mach_factor`.
    mach_factor: AtomicI64,
    /// Its load average, scaled by `LOAD_SCALE`; written only by
    /// `compute_mach_factor`.
    load_average: AtomicI64,
    /// Its load, scaled by `SCHEDULER_SCALE`; written only by
    /// `compute_mach_factor`, and read without its token.
    load: AtomicU32,
    /// Its port, for controlling it.
    port: AtomicPtr<Port>,
    /// Its name port, for information about it.
    name_port: AtomicPtr<Port>,
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
    pub AllSetsAdapter = &'static ProcessorSet: ProcessorSet { all_link => LinkedListAtomicLink }
);

impl ProcessorSet {
    /// A new set, not in use yet, with one reference, no processor and no
    /// task.
    pub(super) const fn new() -> Self {
        Self {
            active: AtomicBool::new(false),
            empty: AtomicBool::new(true),
            processor_count: AtomicU32::new(0),
            adjust_index: AtomicU32::new(0),
            references: Count::new(1),
            max_priority: SYSTEM_PRIORITY,
            policies: Policies::TIMESHARE,
            quantum: AtomicU32::new(MIN_QUANTUM),
            machine_quantum: [const { AtomicU32::new(MIN_QUANTUM) }; MAX_CPUS + 1],
            mach_factor: AtomicI64::new(0),
            load_average: AtomicI64::new(0),
            load: AtomicU32::new(SCHEDULER_SCALE),
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

    /// Whether it is in use; changed only under `members`' token.
    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// Whether it has no processor; changed only under `members`' token.
    pub(crate) fn is_empty(&self) -> bool {
        self.empty.load(Ordering::Relaxed)
    }

    /// How many processors it has; changed only under `members`' token.
    pub(crate) fn processor_count(&self) -> u32 {
        self.processor_count.load(Ordering::Relaxed)
    }

    /// The highest priority its threads may take.
    pub(crate) const fn max_priority(&self) -> Priority {
        self.max_priority
    }

    /// The policies its threads may use.
    pub(crate) const fn policies(&self) -> Policies {
        self.policies
    }

    /// Its processors' quantum, in ticks, as last computed.
    pub(crate) fn quantum(&self) -> u32 {
        self.quantum.load(Ordering::Relaxed)
    }

    /// Its load average, scaled by `LOAD_SCALE`.
    pub(crate) fn load_average(&self) -> i64 {
        self.load_average.load(Ordering::Relaxed)
    }

    /// Its load, scaled by `SCHEDULER_SCALE`.
    pub(crate) fn load(&self) -> u32 {
        self.load.load(Ordering::Relaxed)
    }

    /// Moves its Mach factor and load average a fifth of the way to
    /// `factor` and `average`, and its load half way to `load`; by
    /// `compute_mach_factor` only, under `members`' token.
    pub(crate) fn update_averages(&self, factor: i64, average: i64, load: u32) {
        let mach_factor = self.mach_factor.load(Ordering::Relaxed);
        self.mach_factor
            .store((4 * mach_factor + factor) / 5, Ordering::Relaxed);
        let load_average = self.load_average.load(Ordering::Relaxed);
        self.load_average
            .store((4 * load_average + average) / 5, Ordering::Relaxed);
        let old = self.load.load(Ordering::Relaxed);
        self.load.store(old.midpoint(load), Ordering::Relaxed);
    }

    /// Sets its port, once, before others reach it; the slot keeps its
    /// reference.
    pub(crate) fn set_port(&self, port: Reference<Port>) {
        self.port
            .store(Reference::into_raw(port).as_ptr(), Ordering::Release);
    }

    /// Sets its name port, once, before others reach it; the slot keeps its
    /// reference.
    pub(crate) fn set_name_port(&self, port: Reference<Port>) {
        self.name_port
            .store(Reference::into_raw(port).as_ptr(), Ordering::Release);
    }

    /// Takes its token.
    pub(crate) fn lock(&self) -> TokenGuard<'_, Members> {
        self.members.lock()
    }

    /// A send right for `set`'s port, made as `convert_pset_to_port` makes
    /// it; `set`'s reference drops after.
    ///
    /// # Panics
    ///
    /// If it is out of use: not yet.
    pub fn port(set: Reference<Self>) -> Reference<Port> {
        let members = set.members.lock();
        assert!(
            set.is_active(),
            "convert_pset_to_port: a set out of use, not yet"
        );
        let port =
            NonNull::new(set.port.load(Ordering::Acquire)).expect("convert_pset_to_port: no port");
        // SAFETY: the slot keeps its port's reference while the set is in
        // use, which this handle borrows and never drops.
        let port = ManuallyDrop::new(unsafe { Reference::from_raw(port) });
        let send = Port::make_send(&port);
        drop(members);
        drop(set);
        send
    }

    /// A send right for `set`'s name port, made as
    /// `convert_pset_name_to_port` makes it; `set`'s reference drops after.
    ///
    /// # Panics
    ///
    /// If it is out of use: not yet.
    pub fn name_port(set: Reference<Self>) -> Reference<Port> {
        let members = set.members.lock();
        assert!(
            set.is_active(),
            "convert_pset_name_to_port: a set out of use, not yet"
        );
        let port = NonNull::new(set.name_port.load(Ordering::Acquire))
            .expect("convert_pset_name_to_port: no name port");
        // SAFETY: the slot keeps its port's reference while the set is in
        // use, which this handle borrows and never drops.
        let port = ManuallyDrop::new(unsafe { Reference::from_raw(port) });
        let send = Port::make_send(&port);
        drop(members);
        drop(set);
        send
    }

    /// A new set, in use, in the list of all sets, with its ports and
    /// references for them, and for the caller's set and name.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no set can be allocated.
    pub fn create() -> Result<&'static Self, Error> {
        // Its own reference, and the caller's set's and name's.
        let mut set = SET_CACHE.allocate(Self {
            references: Count::new(3),
            ..Self::new()
        })?;
        *set.active.get_mut() = true;
        let set = CacheBox::into_raw(set);
        // The box's provenance, which `free` takes back from a handle made
        // from a shared borrow.
        set.expose_provenance();
        // SAFETY: a set lives until it is destroyed, which comes later.
        let set = unsafe { set.as_ref() };
        ipc_host::init_set(set);
        set.join_all();
        Ok(set)
    }

    /// Ends it, as `processor_set_destroy` does: out of use, its ports
    /// standing for nothing, then destroyed, so that their rights name dead
    /// ports, and its own reference dropped; the caller holds another.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for the default set; `Failure` if it is out of use
    /// already.
    ///
    /// # Panics
    ///
    /// If it has tasks, threads or processors, which go to the default set:
    /// not yet. As `Port::deallocate_kernel`.
    pub fn destroy(&self) -> Result<(), Error> {
        if ptr::eq(self, &raw const DEFAULT_SET) {
            return Err(Error::InvalidArgument);
        }
        let members = self.members.lock();
        if !self.active.swap(false, Ordering::Relaxed) {
            drop(members);
            return Err(Error::Failure);
        }
        // As ipc_pset_disable: each port stands for nothing, and the
        // reference it kept drops, never the last.
        for slot in [&self.port, &self.name_port] {
            // SAFETY: the slot keeps its port's reference until below.
            unsafe { slot.load(Ordering::Acquire).as_ref() }
                .expect("ipc_pset_disable: no port")
                .replace_kernel_object(None);
            // SAFETY: the reference the port kept, from `ipc_host::init_set`.
            drop(unsafe { Reference::from_raw(NonNull::from(self)) });
        }
        assert!(
            members.task_count == 0,
            "processor_set_destroy: task_assign, not yet"
        );
        assert!(
            members.thread_count == 0,
            "processor_set_destroy: thread_assign, not yet"
        );
        assert!(
            self.processor_count() == 0,
            "processor_set_destroy: processor_assign, not yet"
        );
        drop(members);
        // As ipc_pset_terminate: each port destroyed, then its slot's
        // reference dropped.
        for slot in [&self.port, &self.name_port] {
            let port = NonNull::new(slot.swap(ptr::null_mut(), Ordering::AcqRel))
                .expect("ipc_pset_terminate: no port");
            // SAFETY: the slot's reference, which this handle takes.
            let port = unsafe { Reference::from_raw(port) };
            port.deallocate_kernel();
        }
        // SAFETY: its own reference, from `create`; never the last, as the
        // caller holds one.
        drop(unsafe { Reference::from_raw(NonNull::from(self)) });
        Ok(())
    }

    /// Writes its scheduling information into `info`, as
    /// `processor_set_info`'s `PROCESSOR_SET_SCHED_INFO` does; `info`'s
    /// length is the count asked for. Gives the count written.
    ///
    /// # Errors
    ///
    /// `Failure` for a count short of the record.
    pub(crate) fn sched_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        // Mach's processor_set_sched_info: policies, then max_priority.
        let Some(sched) = info.first_chunk_mut::<2>() else {
            return Err(Error::Failure);
        };
        // Fixed when it is made, so read without its token.
        *sched = [
            i32::try_from(self.policies.bits()).unwrap(),
            i32::try_from(u32::from(self.max_priority)).unwrap(),
        ];
        Ok(sched.len())
    }

    /// Takes `thread`, new and in no set, into its `members`; the thread
    /// keeps the reference the caller took for it.
    pub(crate) fn add_thread(&self, members: &mut Members, thread: &'static Thread) {
        members.threads.push_back(thread);
        thread.set_processor_set(Some(self));
        members.thread_count += 1;
    }

    /// Lets `thread`, being freed, out of `members`, its set's.
    pub(crate) fn remove_thread(members: &mut Members, thread: &Thread) {
        // SAFETY: the thread is on this list, which it joined as it was
        // created.
        unsafe { members.threads.cursor_mut_from_ptr(thread) }.remove();
        thread.set_processor_set(None);
        members.thread_count -= 1;
    }

    /// Takes `processor`, coming up, and sets its quanta again; the caller
    /// holds `members`' token.
    pub(crate) fn add_processor(&'static self, processor: &'static Processor) {
        processor
            .set
            .store(ptr::from_ref(self).cast_mut(), Ordering::Release);
        let count = self.processor_count.fetch_add(1, Ordering::Relaxed) + 1;
        self.empty.store(false, Ordering::Relaxed);
        self.set_quantum(count);
    }

    /// Sets its quanta for `count` processors: an even share of the
    /// shortest quantum each, by threads waiting; then its quantum for the
    /// threads waiting now, up to one each.
    fn set_quantum(&self, count: u32) {
        for waiting in 1..=count {
            self.machine_quantum[usize::try_from(waiting).unwrap()].store(
                (MIN_QUANTUM * count + waiting / 2) / waiting,
                Ordering::Relaxed,
            );
        }
        let one = self.machine_quantum[1].load(Ordering::Relaxed);
        self.machine_quantum[0].store(2 * one, Ordering::Relaxed);
        let waiting = usize::try_from(self.queued().min(count)).unwrap();
        self.quantum.store(
            self.machine_quantum[waiting].load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
    }

    /// Its quantum for the threads waiting now, up to one each of its
    /// processors.
    pub(crate) fn current_quantum(&self) -> u32 {
        let waiting = self
            .queued()
            .min(self.processor_count.load(Ordering::Relaxed));
        let quantum =
            self.machine_quantum[usize::try_from(waiting).unwrap()].load(Ordering::Relaxed);
        self.quantum.store(quantum, Ordering::Relaxed);
        quantum
    }

    /// How many threads wait on its run queue: those its processors' run
    /// queues hold that are not bound to them, read without their locks.
    pub(crate) fn queued(&self) -> u32 {
        (0..MAX_CPUS)
            .map(|cpu| smp::of(cpu).processor())
            .filter(|processor| processor.set().is_some_and(|set| ptr::eq(set, self)))
            .map(|processor| processor.unbound.load(Ordering::Relaxed))
            .sum()
    }

    /// The best priority among the threads waiting on its run queue.
    ///
    /// # Panics
    ///
    /// If one of its processors is another running CPU's: that comes with
    /// SMP.
    pub(crate) fn best_queued(&self) -> Option<Priority> {
        (0..MAX_CPUS)
            .map(|cpu| smp::of(cpu).processor())
            .filter(|processor| processor.set().is_some_and(|set| ptr::eq(set, self)))
            .filter_map(Processor::best_unbound)
            .reduce(|best, priority| {
                if priority.outranks(best) {
                    priority
                } else {
                    best
                }
            })
    }

    /// `quantum` adjusted at run time: its processors take turns at
    /// quanta spread from the shortest to it, so theirs do not run out
    /// together. It has more than one processor.
    pub(crate) fn adjust_quantum(&self, quantum: u32) -> u32 {
        let count = self.processor_count.load(Ordering::Relaxed);
        let index = self
            .adjust_index
            .update(Ordering::Relaxed, Ordering::Relaxed, |index| {
                if index + 1 >= count { 0 } else { index + 1 }
            });
        MIN_QUANTUM + index * (quantum - MIN_QUANTUM) / (count - 1)
    }

    /// Adds it to the list of all sets.
    pub(super) fn join_all(&'static self) {
        let mut all = ALL_SETS.lock();
        all.list.push_back(self);
        all.count += 1;
        drop(all);
    }

    /// Takes `task`, new and in no set; the task keeps the reference the
    /// caller took for it.
    pub(crate) fn add_task(&self, task: &'static Task) {
        let mut members = self.members.lock();
        members.tasks.push_back(task);
        task.set
            .store(ptr::from_ref(self).cast_mut(), Ordering::Release);
        members.task_count += 1;
        drop(members);
    }

    /// Lets `task`, being freed, out of its tasks, as `pset_remove_task`
    /// does; the caller then drops the task's reference on it.
    ///
    /// # Panics
    ///
    /// If `task` is in another set: not yet.
    pub(crate) fn remove_task(&self, task: &Task) {
        let mut members = self.members.lock();
        assert!(
            ptr::eq(task.set.load(Ordering::Relaxed), self),
            "pset_remove_task: a task in another set, not yet"
        );
        // SAFETY: the task is on this list, which it joined in `add_task`.
        unsafe { members.tasks.cursor_mut_from_ptr(task) }.remove();
        task.set.store(ptr::null_mut(), Ordering::Release);
        members.task_count -= 1;
        drop(members);
    }
}

// SAFETY: its count is its own, and every set but the default one, whose own
// reference never drops, is a box of `SET_CACHE`, which `free` takes back.
unsafe impl Counted for ProcessorSet {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `set`, as `pset_deallocate` does at zero: off the list of all
    /// sets, under its token, which the list's walkers borrow sets under,
    /// then back to its cache.
    ///
    /// # Panics
    ///
    /// For the default set, or a set with tasks, threads or processors.
    unsafe fn free(set: NonNull<Self>) {
        // The box's own provenance, which `create` exposed.
        let set = NonNull::<Self>::with_exposed_provenance(set.addr());
        let mut all = ALL_SETS.lock();
        // SAFETY: nothing else uses it, and it stays allocated until its box
        // drops below.
        let this = unsafe { set.as_ref() };
        let members = this.members.lock();
        assert!(
            !ptr::eq(this, &raw const DEFAULT_SET)
                && members.task_count == 0
                && members.thread_count == 0
                && this.processor_count() == 0,
            "pset_deallocate: destroy default or active pset"
        );
        drop(members);
        // SAFETY: the set is on this list, which it joined in `join_all`.
        unsafe { all.list.cursor_mut_from_ptr(this) }.remove();
        all.count -= 1;
        drop(all);
        // SAFETY: a `SET_CACHE` box `create` gave up, with that box's
        // provenance; nothing uses it after.
        drop(unsafe { CacheBox::from_raw(&SET_CACHE, set) });
    }
}
