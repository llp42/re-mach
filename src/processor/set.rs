// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::{ManuallyDrop, MaybeUninit};
use core::ptr::{self, NonNull};
use core::slice;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicU32, Ordering};

use tailq::{Link, TailQueue};

use crate::config::MAX_CPUS;
use crate::error::Error;
use crate::ipc::port::port::Port;
use crate::ipc_host;
use crate::kalloc;
use crate::reference::{Count, Counted, Reference};
use crate::sched::policy::{MIN_QUANTUM, Policies, SCHEDULER_SCALE};
use crate::sched::priority::{Priority, SYSTEM_PRIORITY};
use crate::slab::CacheBox;
use crate::smp;
use crate::sync::{Token, TokenGuard};
use crate::task::task::Task;
use crate::thread::thread::Thread;

use super::all_sets::{ALL_SETS, DEFAULT_SET, SET_CACHE};
use super::members::Members;
use super::processor::Processor;

/// A set of processors, and the threads that run on them.
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
    all_link: Link,
}

tailq::adapter!(pub AllSetsAdapter = ProcessorSet { all_link });

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
                tasks: TailQueue::new(),
                task_count: 0,
                threads: TailQueue::new(),
                thread_count: 0,
            }),
            all_link: Link::new(),
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

    /// The set it stands for in `name_port`, as `convert_pset_name_to_port`
    /// gives it.
    /// Send rights for every set's name port, as `host_processor_sets`
    /// lists them: each holding its right's reference, and how many.
    ///
    /// # Errors
    ///
    /// As `kalloc::allocate`.
    ///
    /// # Panics
    ///
    /// If a set in the list is no longer in use: not yet.
    pub(crate) fn name_ports() -> Result<Option<NonNull<[usize]>>, Error> {
        let all = ALL_SETS.lock();
        let count = usize::try_from(all.count).unwrap();
        if count == 0 {
            drop(all);
            return Ok(None);
        }
        let memory = kalloc::allocate(count * size_of::<usize>())?;
        // SAFETY: the array's slots, ours alone.
        let slots = unsafe { slice::from_raw_parts_mut(memory.cast::<usize>().as_ptr(), count) };
        for (slot, set) in slots.iter_mut().zip(&all.list) {
            // SAFETY: a set in the list is in use, and its own reference
            // keeps it while the list's token is held.
            let set = unsafe { Reference::upgrade(NonNull::from(set)) }
                .expect("host_processor_sets: a set freed while listed");
            let send = Self::name_port(set);
            *slot = Reference::into_raw(send).as_ptr().expose_provenance();
        }
        drop(all);
        Ok(Some(NonNull::slice_from_raw_parts(memory.cast(), count)))
    }

    /// Send rights for the ports of its tasks, as `processor_set_tasks`
    /// lists them: each holding its right's reference, and how many; none
    /// for no tasks.
    ///
    /// # Errors
    ///
    /// As `kalloc::allocate`.
    ///
    /// # Panics
    ///
    /// If a task is being freed or tasks left while the array was
    /// allocated: not yet. As `Task::convert_to_port`.
    pub(crate) fn task_ports(&self) -> Result<Option<NonNull<[usize]>>, Error> {
        let mut array: Option<NonNull<[usize]>> = None;
        // Allocated with the token released, until it holds every task.
        let members = loop {
            let members = self.members.lock();
            let count = usize::try_from(members.task_count).unwrap();
            if count <= array.map_or(0, NonNull::len) {
                break members;
            }
            drop(members);
            if let Some(array) = array {
                // SAFETY: from `kalloc::allocate` of its size, below; not
                // used again.
                unsafe { kalloc::free(array.cast(), array.len() * size_of::<usize>()) };
            }
            let memory = kalloc::allocate(count * size_of::<usize>())?;
            array = Some(NonNull::slice_from_raw_parts(memory.cast(), count));
        };
        let Some(array) = array else {
            drop(members);
            return Ok(None);
        };
        assert!(
            usize::try_from(members.task_count) == Ok(array.len()),
            "processor_set_tasks: fewer tasks than the array holds, not yet"
        );
        // SAFETY: the array's slots, ours alone.
        let slots = unsafe {
            slice::from_raw_parts_mut(array.cast::<MaybeUninit<usize>>().as_ptr(), array.len())
        };
        // A reference to each task, for its port.
        for (slot, task) in slots.iter_mut().zip(&members.tasks) {
            // SAFETY: a task leaves its set before it is freed.
            let task = unsafe { Reference::upgrade(NonNull::from(task)) }
                .expect("processor_set_tasks: a task being freed, not yet");
            let send = Task::convert_to_port(task);
            slot.write(Reference::into_raw(send).as_ptr().expose_provenance());
        }
        drop(members);
        Ok(Some(NonNull::slice_from_raw_parts(
            array.cast(),
            array.len(),
        )))
    }

    /// Moves `task` from it to `new`, as `pset_remove_task` and
    /// `pset_add_task` do for `task_assign`, both sets' tokens held in
    /// address order.
    pub(crate) fn move_task(&self, new: &'static Self, task: &'static Task) {
        let (first, second) = if ptr::from_ref(self) < ptr::from_ref(new) {
            (self, new)
        } else {
            (new, self)
        };
        let mut first_members = first.members.lock();
        let mut second_members = second.members.lock();
        let (old_members, new_members) = if ptr::eq(first, self) {
            (&mut *first_members, &mut *second_members)
        } else {
            (&mut *second_members, &mut *first_members)
        };
        // SAFETY: the task is on this set's list, which it joined in
        // `add_task`, and both tokens keep it there.
        unsafe { old_members.tasks.remove_ptr(NonNull::from(task)) };
        old_members.task_count -= 1;
        // SAFETY: the task lives while in a set, and only its set's list
        // writes its set link, under both tokens.
        unsafe { new_members.tasks.push_back_ptr(NonNull::from(task)) };
        new_members.task_count += 1;
        task.set
            .store(ptr::from_ref(new).cast_mut(), Ordering::Release);
        drop(second_members);
        drop(first_members);
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

    /// Takes `thread`, new and in no set, into its `members`; the thread
    /// keeps the reference the caller took for it.
    pub(crate) fn add_thread(&self, members: &mut Members, thread: &'static Thread) {
        // SAFETY: the thread lives until `remove_thread`, and only this list
        // writes its set link, under the token.
        unsafe { members.threads.push_back_ptr(NonNull::from(thread)) };
        thread.set_processor_set(Some(self));
        members.thread_count += 1;
    }

    /// Lets `thread`, being freed, out of `members`, its set's.
    pub(crate) fn remove_thread(members: &mut Members, thread: &Thread) {
        // SAFETY: the thread is on this list, which it joined as it was
        // created.
        unsafe { members.threads.remove_ptr(NonNull::from(thread)) };
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
        // SAFETY: a set lives while listed, and only this list writes its
        // link, under the token.
        unsafe { all.list.push_back_ptr(NonNull::from(self)) };
        all.count += 1;
        drop(all);
    }

    /// Takes `task`, new and in no set; the task keeps the reference the
    /// caller took for it.
    pub(crate) fn add_task(&self, task: &'static Task) {
        let mut members = self.members.lock();
        // SAFETY: the task lives until `remove_task`, and only its set's list
        // writes its set link, under the token.
        unsafe { members.tasks.push_back_ptr(NonNull::from(task)) };
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
        unsafe { members.tasks.remove_ptr(NonNull::from(task)) };
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
        unsafe { all.list.remove_ptr(NonNull::from(this)) };
        all.count -= 1;
        drop(all);
        // SAFETY: a `SET_CACHE` box `create` gave up, with that box's
        // provenance; nothing uses it after.
        drop(unsafe { CacheBox::from_raw(&SET_CACHE, set) });
    }
}
