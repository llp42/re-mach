// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::fmt;
use core::ptr::{self, NonNull};
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use intrusive_collections::{LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::ast::Ast;
use crate::clock;
use crate::clock::time::TimeValue;
use crate::clock::timeout::Timeout;
use crate::ipc::port::port::Port;
use crate::name::Name;
use crate::processor::processor::Processor;
use crate::processor::set::ProcessorSet;
use crate::reference::{Count, Counted, Reference};
use crate::sched::{event::Event, policy::Policy, wait::WaitResult};
use crate::slab::CacheBox;
use crate::sync::{CriticalSection, OnceLock, Spinlock};
use crate::task::task::Task;
use crate::vm;
use crate::vm::kernel::KernelStack;
use crate::{arch, smp};

use super::create::CACHE;
use super::machine::MachineThread as _;
use super::ports::ThreadPorts;
use super::state::ThreadState;

/// A thread's timer ticks a second: thread times count microseconds.
pub const TIMER_RATE: u32 = 1_000_000;

/// A thread.
#[expect(dead_code, reason = "thread_freeze reads may_assign")]
pub struct Thread {
    /// Its place in the sleep queue it waits on.
    pub(super) sleep_link: LinkedListAtomicLink,
    /// Its place in the run queue it waits on.
    pub(super) run_link: LinkedListAtomicLink,
    /// Its place in its task's threads.
    pub(super) task_link: LinkedListAtomicLink,
    /// Its place in its set's threads.
    pub(super) set_link: LinkedListAtomicLink,
    /// Its place in the queue of a port it waits on.
    pub(super) ipc_link: LinkedListAtomicLink,
    /// How many references it has.
    pub(super) references: Count,
    /// Its task.
    pub(super) task: UnsafeRef<Task>,
    /// What it is doing, as `ThreadState` bits.
    pub(super) state: AtomicU32,
    /// The address of the event it waits for; 0 for none.
    pub(super) wait_event: AtomicUsize,
    /// How its last wait ended, a `WaitResult`.
    pub(super) wait_result: AtomicU8,
    /// What ends its wait once some ticks have passed.
    pub(super) timer: Timeout,
    /// How many times it is suspended.
    pub(super) suspend_count: AtomicU32,
    /// How many of those suspensions user tasks asked for.
    pub(super) user_stop_count: AtomicU32,
    /// Its base priority.
    pub(super) priority: AtomicU32,
    /// The highest priority it may take.
    pub(super) max_priority: AtomicU32,
    /// The priority it is scheduled at, its base aged by its use.
    pub(super) sched_pri: AtomicU32,
    /// Its scheduling policy.
    pub(super) policy: Policy,
    /// Its recent processor use, scaled.
    pub(super) cpu_usage: AtomicU32,
    /// Its use, as its priority ages it.
    pub(super) sched_usage: AtomicU32,
    /// The scheduler tick its use was last aged at.
    pub(super) sched_stamp: AtomicU32,
    /// Whether it is alive.
    pub(super) active: AtomicBool,
    /// The processor set it runs on; set as the set takes it.
    pub(super) set: AtomicPtr<ProcessorSet>,
    /// Whether it may be moved to another processor set.
    pub(super) may_assign: bool,
    /// When it was created.
    pub(super) creation_time: TimeValue,
    /// Its name; racing sets may mix two, as in GNU Mach.
    pub(super) name: Name,
    /// Its machine state.
    pub(super) machine_state: UnsafeRef<arch::MachineState>,
    /// Its port, which controls it.
    pub(super) self_port: AtomicPtr<Port>,
    /// The send right for its port it hands out.
    pub(super) self_send: AtomicPtr<Port>,
    /// Its other ports.
    pub(crate) ports: Spinlock<ThreadPorts>,
    /// What its function is given by whoever starts it; null for nothing.
    pub(super) argument: AtomicPtr<()>,
    /// Its kernel stack, which it keeps for life; set as it is created.
    pub(super) kernel_stack: OnceLock<KernelStack>,
    /// The function it starts in; set as it is started.
    pub(super) start_function: OnceLock<fn() -> !>,
    /// The processor it last ran on; null before it first runs.
    pub(super) last_processor: AtomicPtr<Processor>,
    /// Whether a thread waits for it to stop, on the event of this flag.
    pub(super) wake_active: AtomicBool,
    /// The processor it must run on; null for any.
    pub(super) bound: AtomicPtr<Processor>,
    /// How deep its critical sections nest; a new thread starts inside the
    /// one the switch to it is made in.
    pub(super) critical_nesting: AtomicU32,
    /// Its time in user mode, in microseconds.
    pub(super) user_time: AtomicU64,
    /// Its time in the kernel, in microseconds.
    pub(super) system_time: AtomicU64,
    /// Its time in user mode when its use was last charged.
    pub(super) charged_user_time: AtomicU64,
    /// Its time in the kernel when its use was last charged.
    pub(super) charged_system_time: AtomicU64,
    /// Its CPU time since its use was last aged, in microseconds.
    pub(super) cpu_delta: AtomicU32,
    /// The same, scaled by its set's load.
    pub(super) sched_delta: AtomicU32,
    /// The priority its depression ends at; negative when not depressed.
    pub(super) depressed_from: AtomicI32,
    /// Its quantum, in ticks, under the fixed-priority policy.
    pub(super) fixed_quantum: u32,
    /// Its ASTs, which the CPU running it takes.
    pub(super) asts: AtomicU32,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub SleepQueueAdapter = &'static Thread: Thread { sleep_link => LinkedListAtomicLink }
);

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub RunQueueAdapter = &'static Thread: Thread { run_link => LinkedListAtomicLink }
);

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub TaskThreadAdapter = &'static Thread: Thread { task_link => LinkedListAtomicLink }
);

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub SetThreadAdapter = &'static Thread: Thread { set_link => LinkedListAtomicLink }
);

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub IpcQueueAdapter = &'static Thread: Thread { ipc_link => LinkedListAtomicLink }
);

impl Thread {
    /// Its task, which lives while it has threads.
    pub(crate) fn task(&self) -> &Task {
        &self.task
    }

    /// Whether it is alive.
    pub(crate) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }

    /// What it is doing.
    pub(crate) fn state(&self) -> ThreadState {
        ThreadState::from_bits_retain(self.state.load(Ordering::Relaxed))
    }

    /// Adds `state` to what it is doing.
    pub(crate) fn insert_state(&self, state: ThreadState) {
        self.state.fetch_or(state.bits(), Ordering::Relaxed);
    }

    /// Takes `state` off what it is doing.
    pub(crate) fn remove_state(&self, state: ThreadState) {
        self.state.fetch_and(!state.bits(), Ordering::Relaxed);
    }

    /// Sets what it is doing.
    pub(crate) fn set_state(&self, state: ThreadState) {
        self.state.store(state.bits(), Ordering::Relaxed);
    }

    /// The event it waits for, if any.
    pub(crate) fn wait_event(&self) -> Option<Event> {
        Event::try_from(self.wait_event.load(Ordering::Relaxed)).ok()
    }

    /// Sets the event it waits for, or none.
    pub(crate) fn set_wait_event(&self, event: Option<Event>) {
        self.wait_event
            .store(event.map_or(0, usize::from), Ordering::Relaxed);
    }

    /// How its last wait ended.
    pub(crate) fn wait_result(&self) -> WaitResult {
        WaitResult::try_from(self.wait_result.load(Ordering::Relaxed)).unwrap()
    }

    /// Sets how its wait ended.
    pub(crate) fn set_wait_result(&self, result: WaitResult) {
        self.wait_result.store(result.number(), Ordering::Relaxed);
    }

    /// Whether it is on the queue of a port it waits on.
    pub(crate) fn is_queued_to_receive(&self) -> bool {
        self.ipc_link.is_linked()
    }

    /// Its timer.
    pub(crate) const fn timer(&self) -> &Timeout {
        &self.timer
    }

    /// Whether it runs on `set`.
    pub(crate) fn is_in(&self, set: &ProcessorSet) -> bool {
        ptr::eq(self.set.load(Ordering::Acquire), set)
    }

    /// The set it runs on, if any.
    pub(crate) fn processor_set(&self) -> Option<&'static ProcessorSet> {
        // SAFETY: a set lives while it has threads.
        unsafe { self.set.load(Ordering::Acquire).as_ref() }
    }

    /// Sets the set it runs on, or none, under that set's token.
    pub(crate) fn set_processor_set(&self, set: Option<&ProcessorSet>) {
        self.set.store(
            set.map_or(ptr::null_mut(), |set| ptr::from_ref(set).cast_mut()),
            Ordering::Release,
        );
    }

    /// Sets its name to `arguments`, cut to fit.
    pub(crate) fn set_name(&self, arguments: fmt::Arguments) {
        self.name.set(arguments);
    }

    /// Its machine state.
    pub(crate) fn machine_state(&self) -> &arch::MachineState {
        &self.machine_state
    }

    /// What its function is given; null for nothing.
    pub(crate) fn argument(&self) -> *mut () {
        self.argument.load(Ordering::Acquire)
    }

    /// Sets what its function is given, before it starts.
    pub(crate) fn set_argument(&self, argument: *mut ()) {
        self.argument.store(argument, Ordering::Release);
    }

    /// Its ASTs, which the CPU running it takes.
    pub(crate) fn asts(&self) -> Ast {
        Ast::from_bits_retain(self.asts.load(Ordering::Relaxed))
    }

    /// Posts `asts` on it.
    pub(super) fn add_asts(&self, asts: Ast) {
        self.asts.fetch_or(asts.bits(), Ordering::Relaxed);
    }

    /// Clears `asts` on it.
    pub(super) fn remove_asts(&self, asts: Ast) {
        self.asts.fetch_and(!asts.bits(), Ordering::Relaxed);
    }
}

// SAFETY: its count is its own, and every thread is a box of `CACHE`, which
// `free` takes back.
unsafe impl Counted for Thread {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `thread`, as `thread_deallocate` does at zero: its timer
    /// cancelled, its times added to its task's, it leaves its task and its
    /// set, its kernel stack and machine state are freed, then it goes back
    /// to its cache. GNU Mach's recheck under the tokens is not needed: a new
    /// reference comes only from a live one or from `Reference::upgrade`,
    /// which never counts up from zero.
    ///
    /// # Panics
    ///
    /// If it is the current thread, or not stopped. If it moved to another
    /// set: not yet. As `reset_timeout_check` and `vm::kernel::free_stack`.
    unsafe fn free(thread: NonNull<Self>) {
        // The box's own provenance, which `create` exposed.
        let thread = NonNull::<Self>::with_exposed_provenance(thread.addr());
        // SAFETY: nothing else uses it, and it stays allocated until its
        // box drops below.
        let this = unsafe { thread.as_ref() };
        // SAFETY: a thread's set lives while the thread is in it.
        let set = unsafe { &*this.set.load(Ordering::Acquire) };
        let mut set_members = set.lock();
        assert!(
            this.is_in(set),
            "thread_deallocate: a thread that moved, not yet"
        );
        let task = this.task();
        let mut members = task.members.lock();
        let section = CriticalSection::enter();
        clock::timeout::reset_timeout_check(&this.timer);
        task.total_user_time
            .fetch_add(this.user_time.load(Ordering::Relaxed), Ordering::Relaxed);
        task.total_system_time
            .fetch_add(this.system_time.load(Ordering::Relaxed), Ordering::Relaxed);
        let reference = task.remove_thread(&mut members, this);
        ProcessorSet::remove_thread(&mut set_members, this);
        drop(section);
        drop(members);
        drop(set_members);
        // Never the last: a set keeps its own reference until destroyed.
        // SAFETY: the thread's reference, taken in `create`, given up here.
        drop(unsafe { Reference::from_raw(NonNull::from(set)) });
        assert!(
            !smp::current()
                .active_thread()
                .is_some_and(|current| ptr::eq(current, this)),
            "thread deallocating itself"
        );
        assert!(
            this.state()
                .difference(ThreadState::RUN | ThreadState::HALTED)
                == ThreadState::SUSPENDED,
            "unstopped thread destroyed!"
        );
        // task_deallocate: the thread's reference to its task.
        drop(reference);
        // stack_free, with no token or critical section held: the free takes
        // the kernel map's lock and waits for a shootdown.
        // SAFETY: the thread is not the current one and has stopped for good:
        // the reaper, or thread_terminate's halt, saw it off its CPU (on that
        // same CPU; lock note 10.3) before dropping the dead list's or its
        // being alive's reference; this runs once.
        unsafe { vm::kernel::free_stack(this.kernel_stack.get().expect("stack_free: no stack")) };
        // No event counter exists for evc_notify_abort.
        let machine_state = NonNull::new(UnsafeRef::into_raw(this.machine_state.clone())).unwrap();
        // SAFETY: the box `create` gave up, through the pointer it kept; the
        // thread was switched from for the last time, as for its stack, and
        // nothing uses its machine state after; this runs once.
        unsafe { arch::MachineState::terminate(machine_state) };
        // SAFETY: a `CACHE` box `create` gave up, with that box's provenance;
        // nothing uses it after.
        drop(unsafe { CacheBox::from_raw(&CACHE, thread) });
    }
}
