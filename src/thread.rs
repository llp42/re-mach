// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads.

use core::fmt;
use core::mem;
use core::ops::Range;
use core::ptr::{self, NonNull};
use core::slice;
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use intrusive_collections::{LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::ast::Ast;
use crate::clock;
use crate::clock::time::TimeValue;
use crate::clock::timeout::Timeout;
use crate::config::MAX_CPUS;
use crate::error::Error;
use crate::ipc::port::{KernelObject, Port, SendRight};
use crate::machine::Mode;
use crate::name::Name;
use crate::processor::{DEFAULT_SET, Processor, ProcessorSet};
use crate::reference::{Count, Counted, Reference};
use crate::sched::{
    self, Event, Policies, Policy, Preemption, Priority, SCHEDULER_SCALE, WaitResult,
};
use crate::slab::{Cache, CacheBox};
use crate::sync::{CriticalSection, OnceLock, Spinlock};
use crate::task::Task;
use crate::vm;
use crate::vm::kernel::KernelStack;
use crate::vm::phys_map::MachinePhysMap as _;
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
        /// Stopped at a clean point, on its way out of the kernel.
        const HALTED = 0x10;
        /// A processor's idle thread.
        const IDLE = 0x80;
    }
}

impl ThreadState {
    /// The states that say whether it may run.
    pub(super) const SCHEDULING: Self = Self::WAIT
        .union(Self::SUSPENDED)
        .union(Self::RUN)
        .union(Self::UNINTERRUPTIBLE);
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
pub const TIMER_RATE: u32 = 1_000_000;

/// A thread.
#[expect(dead_code, reason = "thread_freeze reads may_assign")]
pub struct Thread {
    /// Its place in the sleep queue it waits on.
    sleep_link: LinkedListAtomicLink,
    /// Its place in the run queue it waits on.
    run_link: LinkedListAtomicLink,
    /// Its place in its task's threads.
    task_link: LinkedListAtomicLink,
    /// Its place in its set's threads.
    set_link: LinkedListAtomicLink,
    /// Its place in the queue of a port it waits on.
    ipc_link: LinkedListAtomicLink,
    /// How many references it has.
    references: Count,
    /// Its task.
    task: UnsafeRef<Task>,
    /// What it is doing, as `ThreadState` bits.
    state: AtomicU32,
    /// The address of the event it waits for; 0 for none.
    wait_event: AtomicUsize,
    /// How its last wait ended, a `WaitResult`.
    wait_result: AtomicU8,
    /// What ends its wait once some ticks have passed.
    timer: Timeout,
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
    machine_state: UnsafeRef<arch::MachineState>,
    /// Its port, which controls it.
    self_port: AtomicPtr<Port>,
    /// The send right for its port it hands out.
    self_send: AtomicPtr<Port>,
    /// Its other ports.
    pub(super) ports: Spinlock<ThreadPorts>,
    /// What its function is given by whoever starts it; null for nothing.
    argument: AtomicPtr<()>,
    /// Its kernel stack, which it keeps for life; set as it is created.
    kernel_stack: OnceLock<KernelStack>,
    /// The processor it last ran on; null before it first runs.
    last_processor: AtomicPtr<Processor>,
    /// Whether a thread waits for it to stop, on the event of this flag.
    wake_active: AtomicBool,
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

/// A thread's ports under its lock, besides its own port; so far its
/// exception port and its reply port.
pub struct ThreadPorts {
    /// The send right its exceptions go to first, if any.
    pub(super) exception: Option<SendRight>,
    /// The port it receives its kernel calls' replies on, made on first
    /// use; the slot holds the reference it was made with.
    pub(super) reply: Option<Reference<Port>>,
}

/// A thread's special port, by Mach's `THREAD_*_PORT` number.
#[derive(Clone, Copy)]
pub enum SpecialPort {
    /// `THREAD_KERNEL_PORT`: the send right it hands out for its port.
    Kernel,
    /// `THREAD_EXCEPTION_PORT`: its exception port.
    Exception,
}

impl TryFrom<i32> for SpecialPort {
    type Error = i32;

    /// The special port Mach's number `number` names, if any.
    fn try_from(number: i32) -> Result<Self, i32> {
        match number {
            1 => Ok(Self::Kernel),
            3 => Ok(Self::Exception),
            _ => Err(number),
        }
    }
}

/// What a thread is doing, as `thread_info` reports it.
#[derive(Clone, Copy)]
enum RunState {
    /// Running, or waiting to run.
    Running,
    /// Stopped.
    Stopped,
    /// Waiting for an event.
    Waiting,
    /// Waiting uninterruptibly.
    Uninterruptible,
    /// Halted at a clean point.
    Halted,
}

impl TryFrom<ThreadState> for RunState {
    type Error = ThreadState;

    /// The run state `state` shows, in Mach's order; none if it shows none.
    fn try_from(state: ThreadState) -> Result<Self, ThreadState> {
        if state.contains(ThreadState::HALTED) {
            Ok(Self::Halted)
        } else if state.contains(ThreadState::RUN) {
            Ok(Self::Running)
        } else if state.contains(ThreadState::UNINTERRUPTIBLE) {
            Ok(Self::Uninterruptible)
        } else if state.contains(ThreadState::SUSPENDED) {
            Ok(Self::Stopped)
        } else if state.contains(ThreadState::WAIT) {
            Ok(Self::Waiting)
        } else {
            Err(state)
        }
    }
}

/// Mach's `TH_STATE_*` number.
impl From<RunState> for i32 {
    fn from(state: RunState) -> Self {
        match state {
            RunState::Running => 1,
            RunState::Stopped => 2,
            RunState::Waiting => 3,
            RunState::Uninterruptible => 4,
            RunState::Halted => 5,
        }
    }
}

/// Mach's `rpc_time_value_t`.
#[repr(C)]
pub struct RpcTimeValue {
    seconds: i64,
    microseconds: i32,
    /// Padding to 8 bytes, zero.
    padding: i32,
}

/// Cut to microseconds, as `TIME_VALUE64_TO_TIME_VALUE`.
#[expect(
    clippy::fallible_impl_from,
    reason = "a time's nanoseconds are under a second"
)]
impl From<TimeValue> for RpcTimeValue {
    fn from(time: TimeValue) -> Self {
        Self {
            seconds: time.seconds,
            microseconds: i32::try_from(time.nanoseconds / 1000).unwrap(),
            padding: 0,
        }
    }
}

/// Mach's `time_value64_t`.
#[repr(C)]
pub struct TimeValue64 {
    seconds: i64,
    nanoseconds: i64,
}

impl From<TimeValue> for TimeValue64 {
    fn from(time: TimeValue) -> Self {
        Self {
            seconds: time.seconds,
            nanoseconds: time.nanoseconds,
        }
    }
}

/// Mach's `thread_basic_info`: 32 words.
#[repr(C)]
struct BasicInfo {
    user_time: RpcTimeValue,
    system_time: RpcTimeValue,
    /// Recent processor use, per mille.
    cpu_usage: i32,
    base_priority: i32,
    current_priority: i32,
    /// A `RunState`'s number; 0 for none.
    run_state: i32,
    /// `TH_FLAGS_*`.
    flags: i32,
    /// The stops users asked for.
    suspend_count: i32,
    /// Scheduler ticks asleep.
    sleep_time: i32,
    /// Padding to 8 bytes, zero.
    padding: i32,
    creation_time: RpcTimeValue,
    user_time64: TimeValue64,
    system_time64: TimeValue64,
    creation_time64: TimeValue64,
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

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub IpcQueueAdapter = UnsafeRef<Thread>: Thread { ipc_link => LinkedListAtomicLink }
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
    /// Gives its thread, new in the current thread's task, what it inherits
    /// from the current thread.
    ///
    /// # Panics
    ///
    /// If no thread runs on the CPU.
    fn inherit(&self);
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
    /// Writes its thread's user state of `flavor` into `state`, whose
    /// length is the count asked for; gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if `state` is too short for the flavor.
    ///
    /// # Panics
    ///
    /// For a flavor not written here: not yet.
    ///
    /// # Safety
    ///
    /// Its thread is stopped.
    unsafe fn read_state(&self, flavor: i32, state: &mut [u32]) -> Result<usize, Error>;
    /// Whether a thread reads its own state of `flavor` directly, without
    /// being stopped.
    fn reads_own_state(flavor: i32) -> bool;
    /// Sets its thread's user state of `flavor` from `state`, whose length
    /// is the count given.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` if `state` is too short for the flavor, or holds
    /// values the flavor refuses.
    ///
    /// # Panics
    ///
    /// For a flavor, or values, not written here: not yet.
    ///
    /// # Safety
    ///
    /// Its thread is stopped, or is the current thread and `flavor` is one
    /// `writes_own_state` names, or is the current thread in a system call
    /// and nothing borrows its user registers.
    unsafe fn write_state(&self, flavor: i32, state: &[u32]) -> Result<(), Error>;
    /// Whether a thread writes its own state of `flavor` directly, without
    /// being stopped.
    fn writes_own_state(flavor: i32) -> bool;
    /// Sets the current thread's own user state of `flavor` from `state`,
    /// as `thread_set_self_state` does: its system call's result is
    /// `KERN_SUCCESS` first, which the state may replace.
    ///
    /// # Errors
    ///
    /// As `write_state`.
    ///
    /// # Panics
    ///
    /// As `write_state`.
    ///
    /// # Safety
    ///
    /// Its thread is the current thread, in a system call, and nothing
    /// borrows its user registers.
    unsafe fn write_own_state(&self, flavor: i32, state: &[u32]) -> Result<(), Error>;
    /// Frees `state`, with its floating-point state.
    ///
    /// # Safety
    ///
    /// `state` is a box from `new`, given up, and its thread was switched
    /// from for the last time: no CPU uses `state` or holds its
    /// floating-point state, and nothing uses it after.
    unsafe fn terminate(state: NonNull<Self>);
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
    arch::MachineState::module_init();
}

impl Thread {
    /// A new thread of `task`, with a reference for the caller, suspended,
    /// with the kernel stack it keeps for life, in its task's set, or the
    /// default set if that one is not in use, at its task's priority aged by a
    /// full share of a processor, with a port and a send right for it; a
    /// thread made in a set with no processor is suspended once more.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the thread, its machine state or its port can't
    /// be allocated, or as `vm::kernel::allocate_stack` for its stack;
    /// `Failure` if the task is not alive.
    #[expect(
        clippy::too_many_lines,
        reason = "thread_create's steps, in GNU Mach's order"
    )]
    pub fn create(task: &'static Task) -> Result<&'static Self, Error> {
        let mut machine_state = arch::MachineState::new()?;
        let port = Port::allocate_kernel()?;
        // A full share of a processor.
        let sched_usage = TIMER_RATE * SCHEDULER_SCALE;
        let mut thread = CACHE.allocate(Self {
            sleep_link: LinkedListAtomicLink::new(),
            run_link: LinkedListAtomicLink::new(),
            task_link: LinkedListAtomicLink::new(),
            set_link: LinkedListAtomicLink::new(),
            ipc_link: LinkedListAtomicLink::new(),
            // One for being alive, one for the caller.
            references: Count::new(2),
            // SAFETY: a task lives while it has threads.
            task: unsafe { UnsafeRef::from_raw(task) },
            state: AtomicU32::new(ThreadState::SUSPENDED.bits()),
            wait_event: AtomicUsize::new(0),
            wait_result: AtomicU8::new(WaitResult::Awakened.number()),
            timer: Timeout::new(thread_timeout),
            // Set below with the priorities, under the set's and task's tokens.
            suspend_count: AtomicU32::new(0),
            user_stop_count: AtomicU32::new(1),
            priority: AtomicU32::new(0),
            max_priority: AtomicU32::new(0),
            sched_pri: AtomicU32::new(0),
            policy: Policy::Timeshare,
            // Set below, by its set's load average.
            cpu_usage: AtomicU32::new(0),
            sched_usage: AtomicU32::new(sched_usage),
            sched_stamp: AtomicU32::new(sched::scheduler_tick()),
            active: AtomicBool::new(true),
            set: AtomicPtr::new(ptr::null_mut()),
            may_assign: true,
            creation_time: clock::time::stamp(),
            // Its task's, copied below under the task's token.
            name: Name::new(),
            // SAFETY: its box, unused until given up below, outlives the thread.
            machine_state: unsafe { UnsafeRef::from_raw(ptr::from_mut(&mut *machine_state)) },
            self_port: AtomicPtr::new(ptr::null_mut()),
            self_send: AtomicPtr::new(ptr::null_mut()),
            // As ipc_thread_init: no exception port, no reply port.
            ports: Spinlock::new(ThreadPorts {
                exception: None,
                reply: None,
            }),
            argument: AtomicPtr::new(ptr::null_mut()),
            kernel_stack: OnceLock::new(),
            last_processor: AtomicPtr::new(ptr::null_mut()),
            wake_active: AtomicBool::new(false),
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
        // SAFETY: under the task's token its reference keeps its set alive,
        // which this handle borrows and never drops; the one taken here does
        // after.
        let mut set = Reference::clone(&mem::ManuallyDrop::new(unsafe {
            Reference::from_raw(NonNull::new(task.set.load(Ordering::Acquire)).unwrap())
        }));
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
            if ptr::eq(current, Reference::as_ptr(&set).as_ptr()) {
                break (set_members, members);
            }
            // SAFETY: as above, under both tokens.
            let current = Reference::clone(&mem::ManuallyDrop::new(unsafe {
                Reference::from_raw(NonNull::from(current))
            }));
            drop(members);
            drop(set_members);
            // Never the last: a set keeps its own reference until destroyed.
            set = current;
        };
        // Its share at the set's load average, taken as at least 1.
        let load_average = set.load_average().max(i64::from(SCHEDULER_SCALE));
        *thread.cpu_usage.get_mut() = u32::try_from(i64::from(sched_usage) / load_average).unwrap();
        let max_priority = members.max_priority.weaker(set.max_priority());
        *thread.max_priority.get_mut() = u32::from(max_priority);
        let priority = members.priority.weaker(max_priority);
        *thread.priority.get_mut() = u32::from(priority);
        *thread.sched_pri.get_mut() = u32::from(priority.aged_by(sched_usage));
        *thread.suspend_count.get_mut() = members.suspend_count + 1 + u32::from(set.is_empty());
        thread.name.copy_from(&members.name);
        // Nothing is published yet; only the set's reference is undone, and
        // the stack stays taken: this path does not free it.
        if !members.active {
            drop(members);
            drop(set_members);
            drop(set);
            return Err(Error::Failure);
        }
        // The thread keeps its machine state for life.
        CacheBox::into_raw(machine_state);
        let thread = CacheBox::into_raw(thread);
        // The box's provenance, which `free` takes back from a handle made
        // from a shared borrow.
        thread.expose_provenance();
        // SAFETY: a thread lives until its last reference drops.
        let thread = unsafe { thread.as_ref() };
        thread.timer.set_argument(ptr::from_ref(thread).cast());
        let port = Reference::from(port);
        let send = Port::make_send(&port);
        port.set_kernel_object(KernelObject::Thread(NonNull::from(thread)));
        // Each slot keeps its reference.
        thread
            .self_port
            .store(Reference::into_raw(port).as_ptr(), Ordering::Release);
        thread
            .self_send
            .store(Reference::into_raw(send).as_ptr(), Ordering::Release);
        set.add_thread(&mut set_members, thread);
        task.add_thread(&mut members, thread);
        drop(members);
        drop(set_members);
        // The thread keeps the set's reference.
        Reference::into_raw(set);
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
        self.machine_state.attach_stack(stack, function);
    }

    /// A send right for `thread`'s port, made as `convert_thread_to_port`
    /// makes it; `thread`'s reference drops after.
    ///
    /// # Panics
    ///
    /// If it has no port: not yet.
    pub(super) fn convert_to_port(thread: Reference<Self>) -> Reference<Port> {
        let port = NonNull::new(thread.self_port.load(Ordering::Acquire))
            .unwrap_or_else(|| panic!("convert_thread_to_port: no port, not yet"));
        // SAFETY: the slot keeps its port's reference until the thread itself
        // halts (`terminate_ipc`), which this handle borrows and never drops:
        // up to `make_send`'s clone nothing blocks, and user threads share
        // CPU 0.
        let port = mem::ManuallyDrop::new(unsafe { Reference::from_raw(port) });
        let send = Port::make_send(&port);
        drop(thread);
        send
    }

    /// Gives a new send right for its port, with its reference, as
    /// `retrieve_thread_self_fast` does.
    ///
    /// # Panics
    ///
    /// If it has no port. If it hands out a right for another port: not
    /// yet. As `Port::copy_send`.
    pub(super) fn retrieve_self(&self) -> Reference<Port> {
        let port = NonNull::new(self.self_port.load(Ordering::Acquire))
            .expect("retrieve_thread_self_fast: no thread port");
        assert!(
            ptr::eq(self.self_send.load(Ordering::Acquire), port.as_ptr()),
            "retrieve_thread_self_fast: an interposed port, not yet"
        );
        // SAFETY: the slot keeps its port's reference until the thread itself
        // halts (`terminate_ipc`), which this handle borrows and never drops:
        // the current thread halts only after its last trap.
        let port = mem::ManuallyDrop::new(unsafe { Reference::from_raw(port) });
        Port::copy_send(&port)
    }

    /// Sets its special port `which` to `port`, as `thread_set_special_port`
    /// does; the old right is released after its lock.
    ///
    /// # Errors
    ///
    /// `Failure` if it has no port; `port` is released.
    ///
    /// # Panics
    ///
    /// If `which` is its kernel port: not yet.
    pub(super) fn set_special_port(
        &self,
        which: SpecialPort,
        port: Option<SendRight>,
    ) -> Result<(), Error> {
        let mut ports = self.ports.lock();
        // `terminate_ipc` empties its port before it takes the lock, then
        // releases what a slot holds.
        if self.self_port.load(Ordering::Acquire).is_null() {
            drop(ports);
            drop(port);
            return Err(Error::Failure);
        }
        let slot = match which {
            SpecialPort::Exception => &mut ports.exception,
            SpecialPort::Kernel => panic!("thread_set_special_port: THREAD_KERNEL_PORT, not yet"),
        };
        let old = mem::replace(slot, port);
        drop(ports);
        // As ipc_port_release_send.
        drop(old);
        Ok(())
    }

    /// Makes its port stand for nothing, as `ipc_thread_disable` does.
    fn disable_ipc(&self) {
        // SAFETY: the slot keeps its port's reference until the thread itself
        // halts (`terminate_ipc`), which this borrows: the current thread has
        // not, and another cannot run on CPU 0 before this returns.
        if let Some(port) = unsafe { self.self_port.load(Ordering::Acquire).as_ref() } {
            port.replace_kernel_object(None);
        }
    }

    /// Ends its ipc, as `ipc_thread_terminate` does: its slots emptied, its
    /// naked send rights released, its reply port given up and its port
    /// destroyed; the names tasks hold for the port stay, as rights to a dead
    /// port.
    ///
    /// # Panics
    ///
    /// If it has no port: not yet. As `Port::release_send`,
    /// `Port::orphan_reply` and `Port::deallocate_kernel`.
    fn terminate_ipc(&self) {
        // Each slot is emptied before its reference goes: a later read finds
        // no port.
        let port = NonNull::new(self.self_port.swap(ptr::null_mut(), Ordering::AcqRel))
            .unwrap_or_else(|| panic!("ipc_thread_terminate: an ended thread, not yet"));
        // SAFETY: the slot kept the port's reference, given up to this handle.
        let port = unsafe { Reference::from_raw(port) };
        if let Some(send) = NonNull::new(self.self_send.swap(ptr::null_mut(), Ordering::AcqRel)) {
            // SAFETY: the slot kept the send right's reference, given up here.
            Port::release_send(unsafe { Reference::from_raw(send) });
        }
        let mut ports = self.ports.lock();
        let exception = ports.exception.take();
        let reply = ports.reply.take();
        drop(ports);
        // Released after the lock, as it drops.
        drop(exception);
        // Its reply port, given up beside them (SC-D2).
        if let Some(reply) = reply {
            Port::orphan_reply(reply);
        }
        port.deallocate_kernel();
        drop(port);
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
        unsafe { self.machine_state.load() }
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
        // The old physical map is given up once the CPU has left its tables.
        if !ptr::eq(&raw const *self.task, &raw const *next.task) {
            next.task.map.phys_map().activate();
            self.task.map.phys_map().deactivate();
        }
        smp::current().switch_nesting(&next.critical_nesting);
        // SAFETY: as the caller promises, `next`'s stack holds a frame
        // `attach_stack` wrote, if it never ran, or a switch pushed; both
        // counts hold the critical section the CPU is in.
        unsafe { self.machine_state.switch(&next.machine_state, self) }
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

    /// The processor it last ran on, if it has run.
    pub(super) fn last_processor(&self) -> Option<&'static Processor> {
        // SAFETY: processors live for good.
        unsafe { self.last_processor.load(Ordering::Acquire).as_ref() }
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
        let priority = Priority::try_from(self.priority.load(Ordering::Relaxed)).unwrap();
        self.sched_pri
            .store(u32::from(priority.aged_by(usage)), Ordering::Relaxed);
    }

    /// Catches its use up with the scheduler's aging, if it missed any.
    pub(super) fn catch_up(&self) {
        if self.sched_stamp.load(Ordering::Relaxed) != sched::scheduler_tick() {
            self.update_priority();
        }
    }

    /// Whether it is stuck: its scheduling state only `RUN`, and its use last
    /// aged more than one scheduler tick ago.
    pub(super) fn is_stuck(&self) -> bool {
        self.state() & ThreadState::SCHEDULING == ThreadState::RUN
            && sched::scheduler_tick().wrapping_sub(self.sched_stamp.load(Ordering::Relaxed)) > 1
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

    /// Waits until it stops, as `thread_dowait` does: at once if it is
    /// suspended outside a wait or in an interruptible one, or if it is
    /// runnable and suspended, stopped as it leaves the current CPU's run
    /// queues; else it sleeps until the switch that stops it wakes it, and
    /// looks again.
    ///
    /// # Panics
    ///
    /// If it is the current thread. If it is runnable and suspended off the
    /// current CPU's run queues, or to wait for on another CPU: not yet.
    fn wait_until_stopped(&self) {
        assert!(
            !smp::current()
                .active_thread()
                .is_some_and(|current| ptr::eq(current, self)),
            "thread_dowait"
        );
        let section = CriticalSection::enter();
        loop {
            // Acquire: what it did before its switch cleared RUN is seen.
            let state = ThreadState::from_bits_retain(self.state.load(Ordering::Acquire))
                & ThreadState::SCHEDULING;
            match state {
                state
                    if state == ThreadState::SUSPENDED
                        || state == ThreadState::WAIT | ThreadState::SUSPENDED =>
                {
                    break;
                }
                state if state == ThreadState::RUN | ThreadState::SUSPENDED => {
                    // The section keeps it from running until it is off the
                    // queue; not there, it runs on another CPU.
                    assert!(
                        smp::current().processor().remove(self),
                        "thread_dowait: cause_ast_check, not yet"
                    );
                    self.stop();
                    break;
                }
                state
                    if [
                        ThreadState::RUN | ThreadState::SUSPENDED | ThreadState::UNINTERRUPTIBLE,
                        ThreadState::RUN | ThreadState::WAIT | ThreadState::SUSPENDED,
                        ThreadState::RUN
                            | ThreadState::WAIT
                            | ThreadState::SUSPENDED
                            | ThreadState::UNINTERRUPTIBLE,
                        ThreadState::WAIT | ThreadState::SUSPENDED | ThreadState::UNINTERRUPTIBLE,
                    ]
                    .contains(&state) =>
                {
                    // Its wake comes from its own CPU's switch, on that CPU's
                    // sleep queue.
                    assert!(
                        self.last_processor().is_some_and(|processor| ptr::eq(
                            processor,
                            smp::current().processor()
                        )),
                        "thread_dowait: another CPU's thread, not yet"
                    );
                    self.wake_active.store(true, Ordering::Relaxed);
                    sched::assert_wait(Some(Event::of(&self.wake_active)));
                    sched::block();
                }
                // Matching no case, it counts as stopped.
                _ => break,
            }
        }
        drop(section);
    }

    /// Stops it, suspended, as its switch away stops it in
    /// `thread_dispatch`: it no longer runs, and the thread waiting for it
    /// to stop, if any, wakes.
    pub(super) fn stop(&self) {
        // Release: what it did before is seen by `wait_until_stopped`'s
        // Acquire.
        self.state
            .fetch_and(!ThreadState::RUN.bits(), Ordering::Release);
        if self.wake_active.swap(false, Ordering::Relaxed) {
            sched::wakeup(Event::of(&self.wake_active));
        }
    }

    /// Lifts one suspension; a thread no longer suspended is no longer
    /// halted either, and runs unless it waits or already runs.
    ///
    /// # Panics
    ///
    /// As `sched::make_runnable`.
    pub(super) fn release(&'static self) {
        let _section = CriticalSection::enter();
        if self.suspend_count.fetch_sub(1, Ordering::Relaxed) != 1 {
            return;
        }
        let state = self.state.fetch_and(
            !(ThreadState::SUSPENDED | ThreadState::HALTED).bits(),
            Ordering::Relaxed,
        );
        if state & (ThreadState::WAIT | ThreadState::RUN).bits() == 0 {
            self.state
                .fetch_or(ThreadState::RUN.bits(), Ordering::Relaxed);
            sched::make_runnable(self, Preemption::Allowed);
        }
    }

    /// Stops it once more for users, as `thread_suspend` does: the first
    /// stop suspends it, and the current thread blocks on its way out of the
    /// kernel.
    ///
    /// # Panics
    ///
    /// If it waits uninterruptibly, or is another thread the first stop
    /// must wait for: not yet.
    pub(super) fn suspend(&self) {
        let section = CriticalSection::enter();
        assert!(
            !self.state().contains(ThreadState::UNINTERRUPTIBLE),
            "thread_suspend: TH_UNINT, not yet"
        );
        let first = self.user_stop_count.fetch_add(1, Ordering::Relaxed) == 0;
        if first {
            self.hold();
        }
        drop(section);
        if !first {
            return;
        }
        let _section = CriticalSection::enter();
        let cpu = smp::current();
        assert!(
            cpu.active_thread()
                .is_some_and(|current| ptr::eq(current, self)),
            "thread_suspend: thread_dowait, not yet"
        );
        cpu.add_asts(Ast::BLOCK);
    }

    /// Lifts one stop users asked for, and the suspension it holds; a thread
    /// no longer suspended at all is no longer halted either, and runs
    /// unless it waits.
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
            let state = self.state.fetch_and(
                !(ThreadState::SUSPENDED | ThreadState::HALTED).bits(),
                Ordering::Relaxed,
            );
            if state & (ThreadState::WAIT | ThreadState::RUN).bits() == 0 {
                self.state
                    .fetch_or(ThreadState::RUN.bits(), Ordering::Relaxed);
                sched::make_runnable(self, Preemption::Allowed);
            }
        }
        Ok(())
    }

    /// Ends it, as `thread_terminate` does: its port stands for nothing;
    /// the current thread, no longer alive, halts as it leaves the kernel;
    /// another is halted, gives up its ipc and the reference of its being
    /// alive.
    ///
    /// # Errors
    ///
    /// `Failure` if another thread is no longer alive.
    ///
    /// # Panics
    ///
    /// If no thread runs on the CPU. If the current task or thread is no
    /// longer alive: not yet. As `halt` and `terminate_ipc`.
    pub(super) fn terminate(&self) -> Result<(), Error> {
        self.disable_ipc();
        let current = smp::current()
            .active_thread()
            .expect("thread_terminate: no current thread");
        if ptr::eq(current, self) {
            let section = CriticalSection::enter();
            if self.active.swap(false, Ordering::Relaxed) {
                self.add_asts(Ast::TERMINATE);
            }
            smp::current().add_asts(Ast::TERMINATE);
            drop(section);
            return Ok(());
        }
        let members = current.task().members.lock();
        assert!(
            members.active && current.active.load(Ordering::Relaxed),
            "thread_terminate: the current thread being ended, not yet"
        );
        drop(members);
        // Someone else got there first.
        if !self.active.swap(false, Ordering::Relaxed) {
            return Err(Error::Failure);
        }
        self.halt();
        self.terminate_ipc();
        // thread_deallocate: the reference of its being alive.
        // SAFETY: an alive thread holds that reference, given up here, once,
        // as it is no longer alive; the caller's keeps it until the call
        // ends.
        drop(unsafe { Reference::from_raw(NonNull::from(self)) });
        Ok(())
    }

    /// Halts it at a clean point, leaving it suspended, as `thread_halt`
    /// does when it must: once it stops, an interruptible wait it is in
    /// ends, and until it halts itself it is made to run, uninterruptibly,
    /// so that it notices.
    ///
    /// # Panics
    ///
    /// If it is the current thread, or, not halted, is anything but
    /// suspended once stopped. If another thread is halting it: not yet. As
    /// `wait_until_stopped`.
    fn halt(&self) {
        assert!(
            !smp::current()
                .active_thread()
                .is_some_and(|current| ptr::eq(current, self)),
            "thread_halt: trying to halt current thread."
        );
        // SAFETY: the caller's reference keeps it alive through the call, and
        // the run queue holds it only until it stops, before the call returns.
        let thread = unsafe { &*ptr::from_ref(self) };
        let section = CriticalSection::enter();
        if self.state().contains(ThreadState::HALTED) {
            self.suspend_count.fetch_add(1, Ordering::Relaxed);
            drop(section);
            return;
        }
        self.hold();
        assert!(
            !self.asts().contains(Ast::HALT),
            "thread_halt: another thread halting it, not yet"
        );
        self.add_asts(Ast::HALT);
        drop(section);
        loop {
            self.wait_until_stopped();
            let section = CriticalSection::enter();
            // clear_wait's interrupt_only: an uninterruptible wait goes on.
            if !self.state().contains(ThreadState::UNINTERRUPTIBLE) {
                sched::clear_wait(thread, WaitResult::Interrupted);
            }
            if self.state().contains(ThreadState::HALTED) {
                drop(section);
                return;
            }
            // GNU Mach's threads at a clean continuation halt here; re-mach
            // has no swap functions.
            assert!(
                self.state() & ThreadState::SCHEDULING == ThreadState::SUSPENDED,
                "thread_halt"
            );
            self.insert_state(ThreadState::RUN | ThreadState::UNINTERRUPTIBLE);
            sched::make_runnable(thread, Preemption::Forbidden);
            drop(section);
        }
    }

    /// Whether it is to halt or end, as `thread_should_halt` tells.
    pub(super) fn should_halt(&self) -> bool {
        self.asts().intersects(Ast::HALT | Ast::TERMINATE)
    }

    /// Writes `thread`'s machine state of `flavor` into `state`, as
    /// `thread_get_state` does, with `thread` stopped meanwhile; `state`'s
    /// length is the count asked for. Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for no thread or the current one; as
    /// `MachineThread::read_state`.
    ///
    /// # Panics
    ///
    /// For a state the current thread reads directly: not yet. As
    /// `wait_until_stopped`, `MachineThread::read_state` and `release`.
    pub(super) fn read_state(
        thread: Option<&'static Self>,
        flavor: i32,
        state: &mut [u32],
    ) -> Result<usize, Error> {
        let Some(thread) = thread else {
            return Err(Error::InvalidArgument);
        };
        if smp::current()
            .active_thread()
            .is_some_and(|current| ptr::eq(current, thread))
        {
            assert!(
                !arch::MachineState::reads_own_state(flavor),
                "thread_get_state: the current thread's own state, not yet"
            );
            return Err(Error::InvalidArgument);
        }
        thread.hold();
        thread.wait_until_stopped();
        // SAFETY: stopped above, and held so until the release.
        let result = unsafe { thread.machine_state.read_state(flavor, state) };
        thread.release();
        result
    }

    /// Sets `thread`'s machine state of `flavor` from `state`, as
    /// `thread_set_state` does, with `thread` stopped meanwhile unless it
    /// is the current thread and writes that state directly; `state`'s
    /// length is the count given.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for no thread, or the current one with a state it
    /// does not write directly; as `MachineThread::write_state`.
    ///
    /// # Panics
    ///
    /// As `wait_until_stopped`, `MachineThread::write_state` and `release`.
    pub(super) fn write_state(
        thread: Option<&'static Self>,
        flavor: i32,
        state: &[u32],
    ) -> Result<(), Error> {
        let Some(thread) = thread else {
            return Err(Error::InvalidArgument);
        };
        if smp::current()
            .active_thread()
            .is_some_and(|current| ptr::eq(current, thread))
        {
            if arch::MachineState::writes_own_state(flavor) {
                // SAFETY: the current thread, with a state it writes
                // directly.
                return unsafe { thread.machine_state.write_state(flavor, state) };
            }
            return Err(Error::InvalidArgument);
        }
        thread.hold();
        thread.wait_until_stopped();
        // SAFETY: stopped above, and held so until the release.
        let result = unsafe { thread.machine_state.write_state(flavor, state) };
        thread.release();
        result
    }

    /// Writes its basic information into `info`, as `thread_info`'s
    /// `THREAD_BASIC_INFO` does; `info`'s length is the count asked for.
    /// Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a count short of the record without its 64-bit
    /// times.
    pub(super) fn basic_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        let count = size_of::<BasicInfo>() / size_of::<i32>();
        let short_count = mem::offset_of!(BasicInfo, user_time64) / size_of::<i32>();
        if info.len() < short_count {
            return Err(Error::InvalidArgument);
        }
        let state = self.state();
        // As update_priority for a thread not running, without storing.
        let mut cpu_usage = self.cpu_usage.load(Ordering::Relaxed);
        let mut priority = self.sched_pri.load(Ordering::Relaxed);
        let ticks = sched::scheduler_tick().wrapping_sub(self.sched_stamp.load(Ordering::Relaxed));
        if !state.contains(ThreadState::RUN) && ticks != 0 {
            let untaken = |time: &AtomicU64, charged: &AtomicU64| {
                let delta = time
                    .load(Ordering::Relaxed)
                    .saturating_sub(charged.load(Ordering::Relaxed));
                u32::try_from(delta).unwrap_or(u32::MAX)
            };
            let delta = untaken(&self.system_time, &self.charged_system_time)
                .wrapping_add(untaken(&self.user_time, &self.charged_user_time));
            // SAFETY: a thread's set lives while the thread is in it.
            let set = unsafe { &*self.set.load(Ordering::Acquire) };
            let (aged_cpu_usage, aged_sched_usage) = if ticks > 30 {
                (0, 0)
            } else {
                let shifts = AGING[usize::try_from(ticks).unwrap()];
                let cpu_usage = cpu_usage
                    .wrapping_add(self.cpu_delta.load(Ordering::Relaxed))
                    .wrapping_add(delta);
                let sched_usage = self
                    .sched_usage
                    .load(Ordering::Relaxed)
                    .wrapping_add(self.sched_delta.load(Ordering::Relaxed))
                    .wrapping_add(delta.wrapping_mul(set.load()));
                (aged(cpu_usage, shifts), aged(sched_usage, shifts))
            };
            cpu_usage = aged_cpu_usage;
            if self.usage_sets_priority() {
                let base = Priority::try_from(self.priority.load(Ordering::Relaxed)).unwrap();
                priority = u32::from(base.aged_by(aged_sched_usage));
            }
        }
        let (user_time, system_time) = self.read_times();
        let basic = BasicInfo {
            user_time: user_time.into(),
            system_time: system_time.into(),
            // Per mille (TH_USAGE_SCALE), corrected by 3/5 for the 5/8 aging.
            cpu_usage: i32::try_from(cpu_usage / (TIMER_RATE / 1000) * 3 / 5).unwrap(),
            base_priority: i32::try_from(self.priority.load(Ordering::Relaxed)).unwrap(),
            current_priority: i32::try_from(priority).unwrap(),
            run_state: RunState::try_from(state).map_or(0, i32::from),
            // TH_FLAGS_IDLE; never TH_FLAGS_SWAPPED, as stacks are kept.
            flags: if state.contains(ThreadState::IDLE) {
                2
            } else {
                0
            },
            suspend_count: i32::try_from(self.user_stop_count.load(Ordering::Relaxed)).unwrap(),
            // As after update_priority, which makes its stamp current.
            sleep_time: 0,
            padding: 0,
            // As read_time_stamp; no host_set_time yet, so no offset.
            creation_time: self.creation_time.into(),
            user_time64: user_time.into(),
            // The system time, where Mach's copies the user time.
            system_time64: system_time.into(),
            creation_time64: self.creation_time.into(),
        };
        // The 64-bit times only for the whole record's count, as Mach's.
        let written = if info.len() == count {
            count
        } else {
            short_count
        };
        // SAFETY: `count` words of integers, `repr(C)` with no implicit
        // padding.
        let words = unsafe { slice::from_raw_parts(ptr::from_ref(&basic).cast::<i32>(), written) };
        info[..written].copy_from_slice(words);
        Ok(info.len().min(count))
    }

    /// Writes its scheduling information into `info`, as `thread_info`'s
    /// `THREAD_SCHED_INFO` does; `info`'s length is the count asked for.
    /// Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a count short of the record without its last
    /// processor.
    ///
    /// # Panics
    ///
    /// For a count of the record without its last processor, a
    /// fixed-priority thread or one that has run: not yet.
    pub(super) fn sched_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        // Mach's thread_sched_info: policy, data, base, maximum and current
        // priorities, whether depressed, the priority depressed from, last
        // processor; a count one short is allowed.
        if info.len() < 7 {
            return Err(Error::InvalidArgument);
        }
        let Some(sched) = info.first_chunk_mut::<8>() else {
            panic!("thread_info: THREAD_SCHED_INFO_COUNT - 1, not yet");
        };
        assert!(
            matches!(self.policy, Policy::Timeshare),
            "thread_info: POLICY_FIXEDPRI, not yet"
        );
        assert!(
            self.last_processor().is_none(),
            "thread_info: last_processor's slot_num, not yet"
        );
        let depressed_from = self.depressed_from.load(Ordering::Relaxed);
        *sched = [
            i32::try_from(Policies::TIMESHARE.bits()).unwrap(),
            // No data for a timeshared thread.
            0,
            i32::try_from(self.priority.load(Ordering::Relaxed)).unwrap(),
            i32::try_from(self.max_priority.load(Ordering::Relaxed)).unwrap(),
            i32::try_from(self.sched_pri.load(Ordering::Relaxed)).unwrap(),
            i32::from(depressed_from >= 0),
            depressed_from,
            // Never run, so on no processor yet.
            0,
        ];
        Ok(sched.len())
    }

    /// Its user and system times, as `thread_read_times` reads them.
    pub(super) fn read_times(&self) -> (TimeValue, TimeValue) {
        let time = |microseconds: &AtomicU64| {
            let microseconds = microseconds.load(Ordering::Relaxed);
            TimeValue {
                seconds: i64::try_from(microseconds / u64::from(TIMER_RATE)).unwrap(),
                nanoseconds: i64::try_from(microseconds % u64::from(TIMER_RATE) * 1000).unwrap(),
            }
        };
        (time(&self.user_time), time(&self.system_time))
    }

    /// Its task, which lives while it has threads.
    pub(super) fn task(&self) -> &Task {
        &self.task
    }

    /// Whether it is alive.
    pub(super) fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
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

    /// The event it waits for, if any.
    pub(super) fn wait_event(&self) -> Option<Event> {
        Event::try_from(self.wait_event.load(Ordering::Relaxed)).ok()
    }

    /// Sets the event it waits for, or none.
    pub(super) fn set_wait_event(&self, event: Option<Event>) {
        self.wait_event
            .store(event.map_or(0, usize::from), Ordering::Relaxed);
    }

    /// How its last wait ended.
    pub(super) fn wait_result(&self) -> WaitResult {
        WaitResult::try_from(self.wait_result.load(Ordering::Relaxed)).unwrap()
    }

    /// Sets how its wait ended.
    pub(super) fn set_wait_result(&self, result: WaitResult) {
        self.wait_result.store(result.number(), Ordering::Relaxed);
    }

    /// Whether it is on the queue of a port it waits on.
    pub(super) fn is_queued_to_receive(&self) -> bool {
        self.ipc_link.is_linked()
    }

    /// Its timer.
    pub(super) const fn timer(&self) -> &Timeout {
        &self.timer
    }

    /// Sets its base priority, while it runs.
    pub(super) fn set_priority(&self, priority: Priority) {
        self.priority.store(u32::from(priority), Ordering::Relaxed);
    }

    /// The priority it is scheduled at.
    pub(super) fn scheduled_priority(&self) -> Priority {
        Priority::try_from(self.sched_pri.load(Ordering::Relaxed)).unwrap()
    }

    /// Sets the priority it is scheduled at, while on no run queue.
    pub(super) fn set_scheduled_priority(&self, priority: Priority) {
        self.sched_pri.store(u32::from(priority), Ordering::Relaxed);
    }

    /// Its scheduling policy.
    pub(super) const fn policy(&self) -> Policy {
        self.policy
    }

    /// Whether it runs on `set`.
    pub(super) fn is_in(&self, set: &ProcessorSet) -> bool {
        ptr::eq(self.set.load(Ordering::Acquire), set)
    }

    /// Sets the set it runs on, or none, under that set's token.
    pub(super) fn set_processor_set(&self, set: Option<&ProcessorSet>) {
        self.set.store(
            set.map_or(ptr::null_mut(), |set| ptr::from_ref(set).cast_mut()),
            Ordering::Release,
        );
    }

    /// Sets its name to `arguments`, cut to fit.
    pub(super) fn set_name(&self, arguments: fmt::Arguments) {
        self.name.set(arguments);
    }

    /// Its machine state.
    pub(super) fn machine_state(&self) -> &arch::MachineState {
        &self.machine_state
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

    /// Posts `asts` on it.
    fn add_asts(&self, asts: Ast) {
        self.asts.fetch_or(asts.bits(), Ordering::Relaxed);
    }

    /// Clears `asts` on it.
    fn remove_asts(&self, asts: Ast) {
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

/// The thread `port` stands for, with a reference to it taken under the
/// port's lock, as `convert_port_to_thread` gives it.
pub fn of(port: &Port) -> Option<Reference<Thread>> {
    port.with_kernel_object(|object| match object {
        // SAFETY: a thread outlives its port's standing for it.
        Some(KernelObject::Thread(thread)) => unsafe { Reference::upgrade(*thread) },
        _ => None,
    })
}

/// Sets the current thread's base priority to `priority`, its highest
/// raised to it if need be, and its scheduled priority from its use.
///
/// # Panics
///
/// If the thread is not timeshared, or is depressed: those come later.
pub fn set_own_priority(priority: Priority) {
    let _section = CriticalSection::enter();
    let thread = smp::current()
        .active_thread()
        .expect("thread_set_own_priority: no current thread");
    let max_priority = Priority::try_from(thread.max_priority.load(Ordering::Relaxed)).unwrap();
    if priority.outranks(max_priority) {
        thread
            .max_priority
            .store(u32::from(priority), Ordering::Relaxed);
    }
    thread
        .priority
        .store(u32::from(priority), Ordering::Relaxed);
    assert!(
        thread.usage_sets_priority(),
        "compute_priority: fixed priority or depressed, not yet"
    );
    // It runs, so it waits on no run queue.
    thread.compute_my_priority();
}

/// Ends the wait of the thread whose timer `timer` is, as timed out.
fn thread_timeout(timer: &'static Timeout) {
    // SAFETY: only threads' timers are made with this function, each with
    // its thread as its argument, and a thread lives while its timer is set.
    let thread = unsafe { &*timer.argument().cast::<Thread>() };
    // Its home CPU is this one: the default set's threads run on CPU 0, and
    // the other CPUs' bound threads set no timer.
    sched::clear_wait(thread, WaitResult::TimedOut);
}

/// Sets the current thread's timer to end its wait `ticks` ticks after the
/// next one, if it waits.
///
/// # Panics
///
/// If no thread runs on the CPU.
pub fn set_own_timeout(ticks: u64) {
    let _section = CriticalSection::enter();
    let thread = smp::current()
        .active_thread()
        .expect("thread_set_timeout: no current thread");
    if thread.state().contains(ThreadState::WAIT) {
        clock::timeout::set_timeout(&thread.timer, ticks);
    }
}

/// Makes a thread of `task` that starts in `start`, at the system priority,
/// its highest too, and resumes it; the reference its creation gave the
/// caller is dropped.
///
/// # Errors
///
/// As `Thread::create`.
///
/// # Panics
///
/// If it cannot be resumed: a new thread always can.
pub fn kernel_thread(task: &'static Task, start: fn() -> !) -> Result<(), Error> {
    let thread = Thread::create(task)?;
    // SAFETY: the reference its creation gave the caller, dropped here; its
    // own keeps it alive.
    drop(unsafe { Reference::from_raw(NonNull::from(thread)) });
    thread.start(start);
    // Never run, it waits on no run queue.
    let priority = u32::from(sched::SYSTEM_PRIORITY);
    thread.max_priority.store(priority, Ordering::Relaxed);
    thread.priority.store(priority, Ordering::Relaxed);
    thread.sched_pri.store(priority, Ordering::Relaxed);
    thread
        .resume()
        .unwrap_or_else(|error| panic!("kernel_thread: thread_resume: {error:?}"));
    Ok(())
}

/// Halts the current thread on its way out of the kernel, as
/// `thread_halt_self` does: one ending itself gives up its ipc, is held,
/// joins its CPU's dead threads, halted, wakes the reaper and blocks for
/// good; one asked to halt shows it has, and blocks until it is resumed.
///
/// # Panics
///
/// If no thread runs on the CPU, or an ending one runs again after its last
/// block.
pub fn halt_self() {
    let thread = smp::current()
        .active_thread()
        .expect("thread_halt_self: no current thread");
    if !thread.asts().contains(Ast::TERMINATE) {
        // Asked to halt; its resume clears HALTED.
        let section = CriticalSection::enter();
        thread.insert_state(ThreadState::HALTED);
        thread.remove_asts(Ast::HALT);
        drop(section);
        sched::block();
        return;
    }
    thread.terminate_ipc();
    thread.hold();
    // It stays on this CPU until it blocks: the list it joins and the
    // sleep queue woken below are this CPU's.
    let section = CriticalSection::enter();
    // SAFETY: a thread lives while it waits on a dead list: the reaper takes
    // it off before freeing it.
    smp::current()
        .dead_threads()
        .lock()
        .push_back(unsafe { UnsafeRef::from_raw(thread) });
    thread.insert_state(ThreadState::HALTED);
    drop(section);
    sched::wakeup(Event::of(&REAPER));
    sched::block();
    panic!("the zombie walks!");
}

/// What the reaper waits on; only its address counts.
static REAPER: u8 = 0;

/// The reaper's work: it takes each thread ended on any CPU off its dead
/// list, waits until it stops, and drops the reference the list held; with
/// none left it waits for one, uninterruptibly. Each look and
/// the wait are in one critical section, the work outside it.
///
/// # Panics
///
/// As `wait_until_stopped` and `free`.
pub fn reaper_thread() -> ! {
    loop {
        // No thread that ends between the last look and the wait is missed.
        let mut section = CriticalSection::enter();
        while let Some(thread) =
            (0..MAX_CPUS).find_map(|cpu| smp::of(cpu).dead_threads().lock().pop_front())
        {
            drop(section);
            // SAFETY: a dead list holds the reference of its thread's being
            // alive, given up here.
            let thread =
                unsafe { Reference::from_raw(NonNull::new(UnsafeRef::into_raw(thread)).unwrap()) };
            thread.wait_until_stopped();
            drop(thread);
            section = CriticalSection::enter();
        }
        sched::assert_wait(Some(Event::of(&REAPER)));
        sched::block();
        drop(section);
    }
}
