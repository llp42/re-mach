// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads.

use core::fmt;
use core::mem;
use core::ops::Range;
use core::ptr::{self, NonNull};
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use intrusive_collections::{LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::ast::Ast;
use crate::clock::{self, TimeValue};
use crate::error::Error;
use crate::ipc::port::{KernelObject, Port};
use crate::machine::Mode;
use crate::name::Name;
use crate::processor::{DEFAULT_SET, Processor, ProcessorSet};
use crate::sched::{self, Policy, Preemption, RUN_QUEUES, SCHEDULER_SCALE};
use crate::slab::{Cache, CacheBox};
use crate::sync::{CriticalSection, OnceLock};
use crate::task::Task;
use crate::vm;
use crate::vm::kernel::KernelStack;
use crate::vm::pmap::MachinePmap as _;
use crate::{arch, smp};

bitflags::bitflags! {
    /// What a thread is doing, as Mach numbers it; only the states used
    /// here.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct ThreadState: u32 {
        /// Waiting for an event.
        const WAIT = 0x01;
        /// Asked to stop.
        const SUSPENDED = 0x02;
        /// Running, or waiting to run.
        const RUN = 0x04;
        /// Its wait may not be interrupted.
        const UNINTERRUPTIBLE = 0x08;
        /// A processor's idle thread.
        const IDLE = 0x80;
    }
}

/// Whether a thread's quantum ran out at a tick.
#[derive(Clone, Copy)]
pub enum Quantum {
    /// It ran out.
    Expired,
    /// Some of it is left.
    Left,
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
    state: AtomicU32,
    /// The address of the event it waits for; 0 for none.
    wait_event: AtomicUsize,
    /// How many times it is suspended.
    suspend_count: AtomicU32,
    /// How many of those suspensions user tasks asked for.
    user_stop_count: AtomicU32,
    /// Its base priority.
    priority: AtomicU32,
    /// The highest priority it may take.
    max_priority: AtomicU32,
    /// The priority it is scheduled at, its base aged by its use.
    sched_pri: AtomicU32,
    /// Its scheduling policy.
    policy: Policy,
    /// Its recent processor use, scaled.
    cpu_usage: AtomicU32,
    /// Its use, as its priority ages it.
    sched_usage: AtomicU32,
    /// The scheduler tick its use was last aged at.
    sched_stamp: AtomicU32,
    /// Whether it is alive.
    active: AtomicBool,
    /// The processor set it runs on; set as the set takes it.
    set: AtomicPtr<ProcessorSet>,
    /// Whether it may be moved to another processor set.
    may_assign: bool,
    /// When it was created.
    creation_time: TimeValue,
    /// Its name; racing sets may mix two, as in GNU Mach.
    name: Name,
    /// Its machine state.
    pcb: UnsafeRef<arch::Pcb>,
    /// Its port, which controls it.
    self_port: AtomicPtr<Port>,
    /// The send right for its port it hands out.
    self_send: AtomicPtr<Port>,
    /// What its function is given by whoever starts it; null for nothing.
    argument: AtomicPtr<()>,
    /// Its kernel stack, which it keeps for life; set as it is created.
    kernel_stack: OnceLock<KernelStack>,
    /// The processor it last ran on; null before it first runs.
    last_processor: AtomicPtr<Processor>,
    /// The processor it must run on; null for any.
    bound: AtomicPtr<Processor>,
    /// How deep its critical sections nest; a new thread starts inside the
    /// one the switch to it is made in.
    critical_nesting: AtomicU32,
    /// Its time in user mode, in microseconds.
    user_time: AtomicU64,
    /// Its time in the kernel, in microseconds.
    system_time: AtomicU64,
    /// Its time in user mode when its use was last charged.
    charged_user_time: AtomicU64,
    /// Its time in the kernel when its use was last charged.
    charged_system_time: AtomicU64,
    /// Its CPU time since its use was last aged, in microseconds.
    cpu_delta: AtomicU32,
    /// The same, scaled by its set's load.
    sched_delta: AtomicU32,
    /// The priority its depression ends at; negative when not depressed.
    depressed_from: AtomicI32,
    /// Its quantum, in ticks, under the fixed-priority policy.
    fixed_quantum: u32,
    /// Its ASTs, which the CPU running it takes.
    asts: AtomicU32,
}

/// 5/8 to the power of a number of seconds, as two shifts: `x >> first`,
/// plus `x >> second`, or minus `x >> -second` when negative.
const AGING: [(u32, i32); 32] = [
    (1, 1),
    (1, 3),
    (1, -3),
    (2, -7),
    (3, 5),
    (3, -5),
    (4, -8),
    (5, 7),
    (5, -7),
    (6, -10),
    (7, 10),
    (7, -9),
    (8, -11),
    (9, 12),
    (9, -11),
    (10, -13),
    (11, 14),
    (11, -13),
    (12, -15),
    (13, 17),
    (13, -15),
    (14, -17),
    (15, 19),
    (16, 18),
    (16, -19),
    (17, 22),
    (18, 20),
    (18, -20),
    (19, 26),
    (20, 22),
    (20, -22),
    (21, -27),
];

/// `usage` aged by `shifts`, one of `AGING`.
const fn aged(usage: u32, (first, second): (u32, i32)) -> u32 {
    if second > 0 {
        (usage >> first) + (usage >> second)
    } else {
        (usage >> first) - (usage >> second.unsigned_abs())
    }
}

/// The priority a thread of base priority `priority` is scheduled at after
/// `usage`, within the run queues.
fn computed_priority(priority: u32, usage: u32) -> u32 {
    (priority + (usage >> (sched::PRIORITY_SHIFT + sched::SCHEDULER_SHIFT)))
        .min(u32::try_from(RUN_QUEUES).unwrap() - 1)
}

/// The time `time` gained since `charged`, which takes it now.
fn take_delta(time: &AtomicU64, charged: &AtomicU64) -> u32 {
    let now = time.load(Ordering::Relaxed);
    let delta = now - charged.swap(now, Ordering::Relaxed);
    u32::try_from(delta).unwrap_or(u32::MAX)
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
    /// Switches the current CPU to its thread for good.
    ///
    /// # Safety
    ///
    /// Its stack is set up, and the CPU is inside a critical section the
    /// thread's count holds.
    unsafe fn load(&self) -> !;
    /// Switches the current CPU from its thread, `current`, to `next`'s;
    /// once its thread runs again, returns the thread switched from then.
    ///
    /// # Safety
    ///
    /// `next`'s stack is set up, and the CPU is inside a critical section
    /// both threads' counts hold.
    unsafe fn switch(&self, next: &Self, current: &'static Thread) -> &'static Thread;
    /// Where a user stack of `size` bytes starts.
    fn user_stack_low(size: usize) -> usize;
    /// Sets the current thread's first user registers: its stack pointer to
    /// room for `argument_size` bytes, rounded up to the stack's alignment,
    /// at the top of `stack`, and its instruction pointer to `entry`; returns
    /// where that room starts.
    ///
    /// # Panics
    ///
    /// If `stack` is not aligned, or no thread runs on the CPU.
    fn set_user_registers(stack: Range<usize>, entry: usize, argument_size: usize) -> usize;
    /// Takes the current CPU's ASTs until none is pending, then returns its
    /// thread to user mode.
    fn return_to_user() -> !;
}

/// Where a new thread begins, before its function: it dispatches `old`, the
/// thread switched from if any, then leaves the critical section the switch
/// to it was made in.
pub extern "sysv64" fn begin(old: Option<&'static Thread>) {
    if let Some(old) = old {
        sched::dispatch(old);
    }
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
    /// A new thread of `task`, suspended, with the kernel stack it keeps for
    /// life, in its task's set, or the default set if that one is not in use,
    /// at its task's priority aged by a full share of a processor, with a port
    /// and a send right for it; a thread made in a set with no processor is
    /// suspended once more.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the thread, its machine state or its port can't
    /// be allocated, or as `vm::kernel::allocate_stack` for its stack;
    /// `Failure` if the task is not alive.
    pub fn create(task: &'static Task) -> Result<&'static Self, Error> {
        let mut pcb = arch::Pcb::new()?;
        let port = Port::allocate_kernel()?;
        port.make_send();
        // A full share of a processor; a set's load average is at least 1.
        let sched_usage = TIMER_RATE * SCHEDULER_SCALE;
        let mut thread = CACHE.allocate(Self {
            sleep_link: LinkedListAtomicLink::new(),
            run_link: LinkedListAtomicLink::new(),
            task_link: LinkedListAtomicLink::new(),
            set_link: LinkedListAtomicLink::new(),
            references: AtomicU32::new(2),
            // SAFETY: a task lives while it has threads.
            task: unsafe { UnsafeRef::from_raw(task) },
            state: AtomicU32::new(ThreadState::SUSPENDED.bits()),
            wait_event: AtomicUsize::new(0),
            // Set below with the priorities, under the set's and task's tokens.
            suspend_count: AtomicU32::new(0),
            user_stop_count: AtomicU32::new(1),
            priority: AtomicU32::new(0),
            max_priority: AtomicU32::new(0),
            sched_pri: AtomicU32::new(0),
            policy: Policy::Timeshare,
            cpu_usage: AtomicU32::new(TIMER_RATE),
            sched_usage: AtomicU32::new(sched_usage),
            sched_stamp: AtomicU32::new(sched::scheduler_tick()),
            active: AtomicBool::new(true),
            set: AtomicPtr::new(ptr::null_mut()),
            may_assign: true,
            creation_time: clock::stamp(),
            // Its task's, copied below under the task's token.
            name: Name::new(),
            // SAFETY: its box, unused until given up below, outlives the thread.
            pcb: unsafe { UnsafeRef::from_raw(ptr::from_mut(&mut *pcb)) },
            self_port: AtomicPtr::new(ptr::null_mut()),
            self_send: AtomicPtr::new(ptr::null_mut()),
            argument: AtomicPtr::new(ptr::null_mut()),
            kernel_stack: OnceLock::new(),
            last_processor: AtomicPtr::new(ptr::null_mut()),
            bound: AtomicPtr::new(ptr::null_mut()),
            critical_nesting: AtomicU32::new(1),
            user_time: AtomicU64::new(0),
            system_time: AtomicU64::new(0),
            charged_user_time: AtomicU64::new(0),
            charged_system_time: AtomicU64::new(0),
            cpu_delta: AtomicU32::new(0),
            sched_delta: AtomicU32::new(0),
            depressed_from: AtomicI32::new(-1),
            fixed_quantum: 0,
            asts: AtomicU32::new(0),
        })?;
        let stack = vm::kernel::allocate_stack()?;
        assert!(
            thread.kernel_stack.set(stack).is_ok(),
            "thread_create: a stack already"
        );
        let members = task.members.lock();
        // SAFETY: under the task's token its reference keeps its set alive, and
        // the one taken here does after.
        let mut set = unsafe { &*task.set.load(Ordering::Acquire) };
        set.reference();
        drop(members);
        // The set's token, then the task's, again if the task changed sets; a
        // set out of use gives way to the default set.
        let (mut set_members, mut members) = loop {
            let set_members = set.lock();
            let members = task.members.lock();
            // SAFETY: as above; a new set gets its reference before the tokens
            // drop.
            let current = unsafe { &*task.set.load(Ordering::Acquire) };
            let current = if current.is_active() {
                current
            } else {
                &DEFAULT_SET
            };
            if ptr::eq(current, set) {
                break (set_members, members);
            }
            current.reference();
            drop(members);
            drop(set_members);
            // Never the last: a set keeps its own reference until destroyed.
            set.deallocate();
            set = current;
        };
        let max_priority = members.max_priority.max(set.max_priority());
        *thread.max_priority.get_mut() = max_priority;
        let priority = members.priority.max(max_priority);
        *thread.priority.get_mut() = priority;
        *thread.sched_pri.get_mut() = computed_priority(priority, sched_usage);
        *thread.suspend_count.get_mut() = members.suspend_count + 1 + u32::from(set.is_empty());
        thread.name.copy_from(&members.name);
        // Nothing is published yet; only the set's reference is undone, and
        // the stack stays taken, as stacks are not freed yet.
        if !members.active {
            drop(members);
            drop(set_members);
            set.deallocate();
            return Err(Error::Failure);
        }
        // The thread takes its machine state for good.
        CacheBox::into_raw(pcb);
        // SAFETY: a thread lives until the reaper frees it, which comes later.
        let thread = unsafe { CacheBox::into_raw(thread).as_ref() };
        port.set_kernel_object(KernelObject::Thread(NonNull::from(thread)));
        let port = CacheBox::into_raw(port).as_ptr();
        thread.self_port.store(port, Ordering::Release);
        thread.self_send.store(port, Ordering::Release);
        set.add_thread(&mut set_members, thread);
        task.add_thread(&mut members, thread);
        drop(members);
        drop(set_members);
        Ok(thread)
    }

    /// Makes it start in `function` once it first runs: its stack is set to
    /// enter it.
    ///
    /// # Panics
    ///
    /// If it has no stack, which `create` gives it.
    pub fn start(&self, function: fn() -> !) {
        let stack = self.kernel_stack.get().expect("thread_start: no stack");
        self.pcb.attach_stack(stack, function);
    }

    /// Records that it runs on the current CPU, as the CPU switches to it.
    pub fn run_here(&'static self) {
        let cpu = smp::current();
        cpu.set_active_thread(self);
        self.last_processor
            .store(ptr::from_ref(cpu.processor()).cast_mut(), Ordering::Release);
    }

    /// Switches the current CPU to it for good, from inside a critical
    /// section, `section`, which it takes over: the CPU gives up what it ran.
    ///
    /// # Panics
    ///
    /// If the CPU holds a spinlock, or the thread is not inside a critical
    /// section, as threads are while switched away from.
    pub fn load(&'static self, section: CriticalSection) -> ! {
        let cpu = smp::current();
        assert!(cpu.spinlocks() == 0, "load_context: spinlocks held");
        assert!(
            self.critical_nesting.load(Ordering::Relaxed) != 0,
            "load_context: thread outside a critical section"
        );
        // The section goes on in the thread, whose count holds it.
        mem::forget(section);
        cpu.switch_nesting(&self.critical_nesting);
        // SAFETY: the thread has its stack set up to start, and the CPU is
        // in a critical section its count holds.
        unsafe { self.pcb.load() }
    }

    /// Switches the current CPU from it to `next`, inside a critical
    /// section, and to `next`'s task's physical map if its task is
    /// another; once it runs again, returns the thread switched from then.
    ///
    /// # Safety
    ///
    /// The CPU runs on its stack, and `next`'s stack holds the frame
    /// `attach_stack` wrote or a switch away from it pushed.
    ///
    /// # Panics
    ///
    /// If `next` is not inside a critical section, as threads are while
    /// switched away from.
    pub(super) unsafe fn switch(&'static self, next: &'static Self) -> &'static Self {
        assert!(
            next.critical_nesting.load(Ordering::Relaxed) != 0,
            "switch_context: thread outside a critical section"
        );
        // The old pmap is given up once the CPU has left its tables.
        if !ptr::eq(&raw const *self.task, &raw const *next.task) {
            next.task.map.pmap().activate();
            self.task.map.pmap().deactivate();
        }
        smp::current().switch_nesting(&next.critical_nesting);
        // SAFETY: as the caller promises, `next`'s stack holds a frame
        // `attach_stack` wrote, if it never ran, or a switch pushed; both
        // counts hold the critical section the CPU is in.
        unsafe { self.pcb.switch(&next.pcb, self) }
    }

    /// Binds it to `processor`, which alone runs it from then on.
    pub fn bind(&self, processor: &'static Processor) {
        let _section = CriticalSection::enter();
        self.bound
            .store(ptr::from_ref(processor).cast_mut(), Ordering::Release);
    }

    /// The processor it is bound to, if any.
    pub(super) fn bound(&self) -> Option<&'static Processor> {
        // SAFETY: processors live for good.
        unsafe { self.bound.load(Ordering::Acquire).as_ref() }
    }

    /// Adds `microseconds` to its time in `mode`, on the CPU it runs on.
    pub fn add_time(&self, microseconds: u32, mode: Mode) {
        let time = match mode {
            Mode::User => &self.user_time,
            Mode::Kernel => &self.system_time,
        };
        time.store(
            time.load(Ordering::Relaxed) + u64::from(microseconds),
            Ordering::Relaxed,
        );
    }

    /// Charges its use at a tick on the CPU running it, with `quantum`
    /// saying whether its quantum ran out: it catches up with the
    /// scheduler's aging, or, when timeshared, gathers the time it ran, and
    /// its priority follows once its quantum runs out or it gathered enough.
    pub fn charge_tick(&self, quantum: Quantum) {
        if self.sched_stamp.load(Ordering::Relaxed) != sched::scheduler_tick() {
            self.update_priority();
        } else if self.usage_sets_priority() {
            self.take_time_delta();
            let delta = self.sched_delta.load(Ordering::Relaxed);
            if matches!(quantum, Quantum::Expired) || delta >= sched::USAGE_THRESHOLD {
                let usage = self.sched_usage.load(Ordering::Relaxed);
                self.sched_usage
                    .store(usage.wrapping_add(delta), Ordering::Relaxed);
                self.sched_delta.store(0, Ordering::Relaxed);
                self.compute_my_priority();
            }
        }
    }

    /// Its next quantum: `quantum` when timeshared, else its own.
    pub const fn next_quantum(&self, quantum: u32) -> u32 {
        match self.policy {
            Policy::Timeshare => quantum,
            Policy::FixedPriority => self.fixed_quantum,
        }
    }

    /// Whether its use sets its priority: it is timeshared and not
    /// depressed.
    fn usage_sets_priority(&self) -> bool {
        matches!(self.policy, Policy::Timeshare) && self.depressed_from.load(Ordering::Relaxed) < 0
    }

    /// Adds the CPU time it ran since its use was last charged to its
    /// deltas, the scheduler's scaled by its set's load. Use counts wrap,
    /// as Mach's do.
    fn take_time_delta(&self) {
        let delta = take_delta(&self.system_time, &self.charged_system_time)
            .wrapping_add(take_delta(&self.user_time, &self.charged_user_time));
        // SAFETY: a thread's set lives while the thread is in it.
        let set = unsafe { &*self.set.load(Ordering::Acquire) };
        let cpu_delta = self.cpu_delta.load(Ordering::Relaxed);
        self.cpu_delta
            .store(cpu_delta.wrapping_add(delta), Ordering::Relaxed);
        let sched_delta = self.sched_delta.load(Ordering::Relaxed);
        self.sched_delta.store(
            sched_delta.wrapping_add(delta.wrapping_mul(set.load())),
            Ordering::Relaxed,
        );
    }

    /// Schedules it at the priority its base priority and use give.
    fn compute_my_priority(&self) {
        let usage = self.sched_usage.load(Ordering::Relaxed);
        self.sched_pri.store(
            computed_priority(self.priority.load(Ordering::Relaxed), usage),
            Ordering::Relaxed,
        );
    }

    /// Catches its use up with the scheduler's aging, if it missed any.
    pub(super) fn catch_up(&self) {
        if self.sched_stamp.load(Ordering::Relaxed) != sched::scheduler_tick() {
            self.update_priority();
        }
    }

    /// Catches its use up with the scheduler's aging: each second missed
    /// ages it by 5/8, and more than 30 forget it.
    ///
    /// # Panics
    ///
    /// If it missed none.
    fn update_priority(&self) {
        let tick = sched::scheduler_tick();
        let ticks = tick.wrapping_sub(self.sched_stamp.load(Ordering::Relaxed));
        assert!(ticks != 0, "update_priority: no tick missed");
        self.sched_stamp.store(tick, Ordering::Relaxed);
        self.take_time_delta();
        let (cpu_usage, sched_usage) = if ticks > 30 {
            (0, 0)
        } else {
            let shifts = AGING[usize::try_from(ticks).unwrap()];
            let cpu_usage = self.cpu_usage.load(Ordering::Relaxed);
            let sched_usage = self.sched_usage.load(Ordering::Relaxed);
            (
                aged(
                    cpu_usage.wrapping_add(self.cpu_delta.load(Ordering::Relaxed)),
                    shifts,
                ),
                aged(
                    sched_usage.wrapping_add(self.sched_delta.load(Ordering::Relaxed)),
                    shifts,
                ),
            )
        };
        self.cpu_usage.store(cpu_usage, Ordering::Relaxed);
        self.sched_usage.store(sched_usage, Ordering::Relaxed);
        self.cpu_delta.store(0, Ordering::Relaxed);
        self.sched_delta.store(0, Ordering::Relaxed);
        if self.usage_sets_priority() {
            self.compute_my_priority();
        }
    }

    /// Marks it running, as a thread that is about to be switched to.
    pub fn mark_running(&self) {
        self.state
            .fetch_or(ThreadState::RUN.bits(), Ordering::Relaxed);
    }

    /// Suspends it once more.
    pub(super) fn hold(&self) {
        let _section = CriticalSection::enter();
        self.suspend_count.fetch_add(1, Ordering::Relaxed);
        self.state
            .fetch_or(ThreadState::SUSPENDED.bits(), Ordering::Relaxed);
    }

    /// Lifts one suspension; a thread no longer suspended runs.
    ///
    /// # Panics
    ///
    /// If, no longer suspended, it waits or runs: not yet.
    pub(super) fn release(&'static self) {
        let _section = CriticalSection::enter();
        if self.suspend_count.fetch_sub(1, Ordering::Relaxed) != 1 {
            return;
        }
        if let Err(state) = self.state.compare_exchange(
            ThreadState::SUSPENDED.bits(),
            ThreadState::RUN.bits(),
            Ordering::Relaxed,
            Ordering::Relaxed,
        ) {
            panic!("thread_release: thread {self:p} in state {state:#x}, not yet");
        }
        sched::make_runnable(self, Preemption::Allowed);
    }

    /// Lifts one stop users asked for, and the suspension it holds; a thread
    /// no longer suspended at all runs, unless it waits.
    ///
    /// # Errors
    ///
    /// `Failure` if users have not stopped it.
    pub fn resume(&'static self) -> Result<(), Error> {
        if self.user_stop_count.load(Ordering::Relaxed) == 0 {
            return Err(Error::Failure);
        }
        if self.user_stop_count.fetch_sub(1, Ordering::Relaxed) == 1
            && self.suspend_count.fetch_sub(1, Ordering::Relaxed) == 1
        {
            let state = self
                .state
                .fetch_and(!ThreadState::SUSPENDED.bits(), Ordering::Relaxed);
            if state & (ThreadState::WAIT | ThreadState::RUN).bits() == 0 {
                self.state
                    .fetch_or(ThreadState::RUN.bits(), Ordering::Relaxed);
                sched::make_runnable(self, Preemption::Allowed);
            }
        }
        Ok(())
    }

    /// Its task, which lives while it has threads.
    pub(super) fn task(&self) -> &Task {
        &self.task
    }

    /// What it is doing.
    pub(super) fn state(&self) -> ThreadState {
        ThreadState::from_bits_retain(self.state.load(Ordering::Relaxed))
    }

    /// Adds `state` to what it is doing.
    pub(super) fn insert_state(&self, state: ThreadState) {
        self.state.fetch_or(state.bits(), Ordering::Relaxed);
    }

    /// Takes `state` off what it is doing.
    pub(super) fn remove_state(&self, state: ThreadState) {
        self.state.fetch_and(!state.bits(), Ordering::Relaxed);
    }

    /// Sets what it is doing.
    pub(super) fn set_state(&self, state: ThreadState) {
        self.state.store(state.bits(), Ordering::Relaxed);
    }

    /// The address of the event it waits for; 0 for none.
    pub(super) fn wait_event(&self) -> usize {
        self.wait_event.load(Ordering::Relaxed)
    }

    /// Sets the event it waits for; 0 for none.
    pub(super) fn set_wait_event(&self, event: usize) {
        self.wait_event.store(event, Ordering::Relaxed);
    }

    /// Sets its base priority, while it runs.
    pub(super) fn set_priority(&self, priority: u32) {
        self.priority.store(priority, Ordering::Relaxed);
    }

    /// The priority it is scheduled at.
    pub(super) fn scheduled_priority(&self) -> u32 {
        self.sched_pri.load(Ordering::Relaxed)
    }

    /// Sets the priority it is scheduled at, while on no run queue.
    pub(super) fn set_scheduled_priority(&self, priority: u32) {
        self.sched_pri.store(priority, Ordering::Relaxed);
    }

    /// Its scheduling policy.
    pub(super) const fn policy(&self) -> Policy {
        self.policy
    }

    /// Whether it runs on `set`.
    pub(super) fn is_in(&self, set: &ProcessorSet) -> bool {
        ptr::eq(self.set.load(Ordering::Acquire), set)
    }

    /// Sets the set it runs on, under that set's token.
    pub(super) fn set_processor_set(&self, set: &'static ProcessorSet) {
        self.set
            .store(ptr::from_ref(set).cast_mut(), Ordering::Release);
    }

    /// Sets its name to `arguments`, cut to fit.
    pub(super) fn set_name(&self, arguments: fmt::Arguments) {
        self.name.set(arguments);
    }

    /// Its machine state.
    pub(super) fn pcb(&self) -> &arch::Pcb {
        &self.pcb
    }

    /// What its function is given; null for nothing.
    pub(super) fn argument(&self) -> *mut () {
        self.argument.load(Ordering::Acquire)
    }

    /// Sets what its function is given, before it starts.
    pub(super) fn set_argument(&self, argument: *mut ()) {
        self.argument.store(argument, Ordering::Release);
    }

    /// Its ASTs, which the CPU running it takes.
    pub(super) fn asts(&self) -> Ast {
        Ast::from_bits_retain(self.asts.load(Ordering::Relaxed))
    }
}

/// Sets the current thread's base priority to `priority`, its highest
/// raised to it if need be, and its scheduled priority from its use.
///
/// # Panics
///
/// If the thread is not timeshared, or is depressed: those come later.
pub fn set_own_priority(priority: u32) {
    let _section = CriticalSection::enter();
    let thread = smp::current()
        .active_thread()
        .expect("thread_set_own_priority: no current thread");
    if priority < thread.max_priority.load(Ordering::Relaxed) {
        thread.max_priority.store(priority, Ordering::Relaxed);
    }
    thread.priority.store(priority, Ordering::Relaxed);
    assert!(
        thread.usage_sets_priority(),
        "compute_priority: fixed priority or depressed, not yet"
    );
    // It runs, so it waits on no run queue.
    thread.compute_my_priority();
}
