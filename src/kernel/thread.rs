// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, Ordering};

use intrusive_collections::{LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::Error;
use super::clock::{self, TimeValue};
use super::processor::{Processor, ProcessorSet};
use super::sched::{self, Policies, RUN_QUEUES, SCHEDULER_SCALE};
use super::slab::{Cache, CacheBox};
use super::task::{self, Task};
use crate::ipc::port::{self, KernelObject, Port};
use crate::sync::{CriticalSection, OnceLock, Token};
use crate::vm;
use crate::vm::kernel::KernelStack;
use crate::{arch, smp};

bitflags::bitflags! {
    /// What a thread is doing, as Mach numbers it; only the states used
    /// here.
    pub struct ThreadState: u32 {
        /// Asked to stop.
        const SUSPENDED = 0x02;
        /// Running, or waiting to run.
        const RUN = 0x04;
    }
}

/// A thread's timer ticks a second: thread times count microseconds.
const TIMER_RATE: u32 = 1_000_000;

/// A thread.
#[expect(dead_code, reason = "the scheduler and thread_info read them")]
pub struct Thread {
    /// Its place in the sleep queue it waits on.
    sleep_link: LinkedListAtomicLink,
    /// Its place in the run queue it waits on.
    run_link: LinkedListAtomicLink,
    /// Its place in its task's threads.
    task_link: LinkedListAtomicLink,
    /// Its place in its set's threads.
    set_link: LinkedListAtomicLink,
    /// How many references it has.
    references: AtomicU32,
    /// Its task.
    task: UnsafeRef<Task>,
    /// What it is doing, as `ThreadState` bits.
    pub(super) state: AtomicU32,
    /// How many times it is suspended.
    pub(super) suspend_count: AtomicU32,
    /// How many of those suspensions user tasks asked for.
    pub(super) user_stop_count: AtomicU32,
    /// Its base priority.
    priority: u32,
    /// The highest priority it may take.
    max_priority: u32,
    /// The priority it is scheduled at, its base aged by its use.
    sched_pri: u32,
    /// Its scheduling policy.
    policy: Policies,
    /// Its recent processor use, scaled.
    cpu_usage: u32,
    /// Its use, as its priority ages it.
    sched_usage: u32,
    /// The scheduler tick its use was last aged at.
    sched_stamp: u32,
    /// Whether it is alive.
    active: AtomicBool,
    /// The processor set it runs on; set as the set takes it.
    pub(super) set: AtomicPtr<ProcessorSet>,
    /// Whether it may be moved to another processor set.
    may_assign: bool,
    /// When it was created.
    creation_time: TimeValue,
    /// Its name, ending with a NUL.
    name: Token<[u8; task::NAME_SIZE]>,
    /// Its machine state.
    pcb: UnsafeRef<arch::Pcb>,
    /// Its port, which controls it.
    self_port: AtomicPtr<Port>,
    /// The send right for its port it hands out.
    self_send: AtomicPtr<Port>,
    /// The function it starts in, once given.
    start: OnceLock<fn() -> !>,
    /// Its kernel stack, once it has one.
    kernel_stack: OnceLock<KernelStack>,
    /// The processor it last ran on; null before it first runs.
    last_processor: AtomicPtr<Processor>,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub SleepQueueAdapter = UnsafeRef<Thread>: Thread { sleep_link => LinkedListAtomicLink }
);

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub RunQueueAdapter = UnsafeRef<Thread>: Thread { run_link => LinkedListAtomicLink }
);

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub TaskThreadAdapter = UnsafeRef<Thread>: Thread { task_link => LinkedListAtomicLink }
);

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub SetThreadAdapter = UnsafeRef<Thread>: Thread { set_link => LinkedListAtomicLink }
);

/// What threads need of the machine.
pub trait MachineThread: Sized {
    /// Sets up threads' machine state, on the boot CPU.
    fn module_init();
    /// A new thread's machine state, which starts it in user mode.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if none can be allocated.
    fn new() -> Result<CacheBox<Self>, Error>;
    /// Sets `stack` up to start its thread in `start`, which the thread
    /// enters by calling `begin`, once the switch to it is made.
    fn attach_stack(&self, stack: &KernelStack, start: fn() -> !);
}

/// Where a new thread begins, before its function: it leaves the critical
/// section the switch to it was made in.
pub extern "sysv64" fn begin() {
    // SAFETY: a switch is made in a critical section no guard holds, and
    // this thread runs its first instructions in it.
    drop(unsafe { CriticalSection::inherit() });
}

/// The cache threads come from.
static CACHE: Cache<Thread> = Cache::new("thread");

/// Sets up threads, on the boot CPU.
pub fn init() {
    CACHE.register();
    arch::Pcb::module_init();
}

impl Thread {
    /// Makes it start in `function` once it first runs.
    ///
    /// # Panics
    ///
    /// If it has a function to start in already.
    pub fn start(&self, function: fn() -> !) {
        assert!(self.start.set(function).is_ok(), "thread_start: twice");
    }

    /// Gives it its kernel stack, set to start it in its function.
    ///
    /// # Errors
    ///
    /// As `vm::kernel::allocate_stack`.
    ///
    /// # Panics
    ///
    /// If it has no function to start in, or a stack already.
    pub fn allocate_stack(&self) -> Result<(), Error> {
        let start = *self
            .start
            .get()
            .expect("thread_doswapin: no start function");
        let stack = vm::kernel::allocate_stack()?;
        self.pcb.attach_stack(&stack, start);
        assert!(
            self.kernel_stack.set(stack).is_ok(),
            "thread_doswapin: twice"
        );
        Ok(())
    }

    /// Records that it runs on the current CPU, as the CPU switches to it.
    pub fn run_here(&'static self) {
        let cpu = smp::current();
        cpu.set_active_thread(self);
        self.last_processor
            .store(ptr::from_ref(cpu.processor()).cast_mut(), Ordering::Release);
    }

    /// Marks it running, as a thread that is about to be switched to.
    pub fn mark_running(&self) {
        self.state
            .fetch_or(ThreadState::RUN.bits(), Ordering::Relaxed);
    }

    /// Lifts one stop users asked for, and the suspension it holds; a thread
    /// no longer suspended at all runs.
    ///
    /// # Errors
    ///
    /// `Failure` if users have not stopped it.
    ///
    /// # Panics
    ///
    /// If it would go on a run queue: that comes with `thread_setrun`.
    pub fn resume(&self) -> Result<(), Error> {
        if self.user_stop_count.load(Ordering::Relaxed) == 0 {
            return Err(Error::Failure);
        }
        if self.user_stop_count.fetch_sub(1, Ordering::Relaxed) == 1
            && self.suspend_count.fetch_sub(1, Ordering::Relaxed) == 1
        {
            let state = self
                .state
                .fetch_and(!ThreadState::SUSPENDED.bits(), Ordering::Relaxed);
            assert!(
                state & ThreadState::RUN.bits() != 0,
                "thread_resume: thread_setrun comes with the scheduler"
            );
        }
        Ok(())
    }

    /// Names it `name`, cut to fit with its final NUL.
    pub fn set_name(&self, name: &str) {
        let mut stored = self.name.lock();
        let length = name.len().min(task::NAME_SIZE - 1);
        stored.fill(0);
        stored[..length].copy_from_slice(&name.as_bytes()[..length]);
        drop(stored);
    }
}

/// A new thread of `task`, suspended, in its task's set, at its task's
/// priority aged by a full share of a processor, with a port and a send
/// right for it; a thread made in a set with no processor is suspended
/// once more.
///
/// # Errors
///
/// `ResourceShortage` if the thread, its machine state or its port can't
/// be allocated.
pub fn create(task: &'static Task) -> Result<&'static Thread, Error> {
    let pcb = arch::Pcb::new()?;
    let mut port = port::allocate_kernel()?;
    port.make_send();
    // SAFETY: a task's set lives while the task is in it.
    let set = unsafe { &*task.set.load(Ordering::Acquire) };
    let max_priority = task.max_priority.max(set.max_priority);
    let priority = task.priority.max(max_priority);
    // A full share of a processor; a set's load average is at least 1.
    let sched_usage = TIMER_RATE * SCHEDULER_SCALE;
    let sched_pri = (priority + (sched_usage >> (sched::PRIORITY_SHIFT + sched::SCHEDULER_SHIFT)))
        .min(u32::try_from(RUN_QUEUES).unwrap() - 1);
    let suspend_count = task.suspend_count.load(Ordering::Relaxed)
        + 1
        + u32::from(set.empty.load(Ordering::Relaxed));
    let thread = CACHE.allocate(Thread {
        sleep_link: LinkedListAtomicLink::new(),
        run_link: LinkedListAtomicLink::new(),
        task_link: LinkedListAtomicLink::new(),
        set_link: LinkedListAtomicLink::new(),
        references: AtomicU32::new(2),
        // SAFETY: a task lives while it has threads.
        task: unsafe { UnsafeRef::from_raw(task) },
        state: AtomicU32::new(ThreadState::SUSPENDED.bits()),
        suspend_count: AtomicU32::new(suspend_count),
        user_stop_count: AtomicU32::new(1),
        priority,
        max_priority,
        sched_pri,
        policy: Policies::TIMESHARE,
        cpu_usage: TIMER_RATE,
        sched_usage,
        sched_stamp: sched::scheduler_tick(),
        active: AtomicBool::new(true),
        set: AtomicPtr::new(ptr::null_mut()),
        may_assign: true,
        creation_time: clock::stamp(),
        name: Token::new(task.name),
        // SAFETY: the thread takes its machine state for good.
        pcb: unsafe { UnsafeRef::from_raw(CacheBox::into_raw(pcb).as_ptr()) },
        self_port: AtomicPtr::new(ptr::null_mut()),
        self_send: AtomicPtr::new(ptr::null_mut()),
        start: OnceLock::new(),
        kernel_stack: OnceLock::new(),
        last_processor: AtomicPtr::new(ptr::null_mut()),
    })?;
    // SAFETY: a thread lives until the reaper frees it, which comes later.
    let thread = unsafe { CacheBox::into_raw(thread).as_ref() };
    set.add_thread(thread);
    task.add_thread(thread);
    port.set_kernel_object(KernelObject::Thread(NonNull::from(thread)));
    let port = CacheBox::into_raw(port).as_ptr();
    thread.self_port.store(port, Ordering::Release);
    thread.self_send.store(port, Ordering::Release);
    Ok(thread)
}
