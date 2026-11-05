// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads.

use core::iter;
use core::mem;
use core::ptr::{self, NonNull};
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, Ordering,
};

use intrusive_collections::{LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::Error;
use super::clock::{self, TimeValue};
use super::processor::{DEFAULT_SET, Processor, ProcessorSet};
use super::sched::{self, Policies, RUN_QUEUES, SCHEDULER_SCALE};
use super::slab::{Cache, CacheBox};
use super::task::{self, Task};
use crate::ipc::port::{self, KernelObject, Port};
use crate::sync::{CriticalSection, OnceLock};
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
    sched_pri: AtomicU32,
    /// Its scheduling policy.
    pub(super) policy: Policies,
    /// Its recent processor use, scaled.
    cpu_usage: AtomicU32,
    /// Its use, as its priority ages it.
    sched_usage: AtomicU32,
    /// The scheduler tick its use was last aged at.
    sched_stamp: AtomicU32,
    /// Whether it is alive.
    active: AtomicBool,
    /// The processor set it runs on; set as the set takes it.
    pub(super) set: AtomicPtr<ProcessorSet>,
    /// Whether it may be moved to another processor set.
    may_assign: bool,
    /// When it was created.
    creation_time: TimeValue,
    /// Its name, ending with a NUL.
    name: [AtomicU8; task::NAME_SIZE],
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
    pub(super) asts: AtomicU32,
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

    /// Adds `microseconds` to its time in user mode or in the kernel, on the
    /// CPU it runs on.
    pub fn add_time(&self, microseconds: u32, user_mode: bool) {
        let time = if user_mode {
            &self.user_time
        } else {
            &self.system_time
        };
        time.store(
            time.load(Ordering::Relaxed) + u64::from(microseconds),
            Ordering::Relaxed,
        );
    }

    /// Charges its use at a tick on the CPU running it, its quantum having
    /// run out or not: it catches up with the scheduler's aging, or, when
    /// timeshared, gathers the time it ran, and its priority follows once
    /// its quantum runs out or it gathered enough.
    pub fn charge_tick(&self, quantum_expired: bool) {
        if self.sched_stamp.load(Ordering::Relaxed) != sched::scheduler_tick() {
            self.update_priority();
        } else if self.usage_sets_priority() {
            self.take_time_delta();
            let delta = self.sched_delta.load(Ordering::Relaxed);
            if quantum_expired || delta >= sched::USAGE_THRESHOLD {
                let usage = self.sched_usage.load(Ordering::Relaxed);
                self.sched_usage
                    .store(usage.wrapping_add(delta), Ordering::Relaxed);
                self.sched_delta.store(0, Ordering::Relaxed);
                self.compute_my_priority();
            }
        }
    }

    /// Its next quantum: `quantum` when timeshared, else its own.
    pub fn next_quantum(&self, quantum: u32) -> u32 {
        if self.policy == Policies::TIMESHARE {
            quantum
        } else {
            self.fixed_quantum
        }
    }

    /// Whether its use sets its priority: it is timeshared and not
    /// depressed.
    fn usage_sets_priority(&self) -> bool {
        self.policy == Policies::TIMESHARE && self.depressed_from.load(Ordering::Relaxed) < 0
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
            sched_delta.wrapping_add(delta.wrapping_mul(set.load)),
            Ordering::Relaxed,
        );
    }

    /// Schedules it at the priority its base priority and use give.
    fn compute_my_priority(&self) {
        let usage = self.sched_usage.load(Ordering::Relaxed);
        self.sched_pri
            .store(computed_priority(self.priority, usage), Ordering::Relaxed);
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
        // Unlocked, as in GNU Mach: racing calls may mix two names.
        let bytes = name.bytes().chain(iter::repeat(0));
        for (stored, byte) in self.name[..task::NAME_SIZE - 1].iter().zip(bytes) {
            stored.store(byte, Ordering::Relaxed);
        }
    }
}

/// A new thread of `task`, suspended, in its task's set, or the default
/// set if that one is not in use, at its task's priority aged by a full
/// share of a processor, with a port and a send right for it; a thread
/// made in a set with no processor is suspended once more.
///
/// # Errors
///
/// `ResourceShortage` if the thread, its machine state or its port can't
/// be allocated; `Failure` if the task is not alive.
pub fn create(task: &'static Task) -> Result<&'static Thread, Error> {
    let mut pcb = arch::Pcb::new()?;
    let mut port = port::allocate_kernel()?;
    port.make_send();
    // A full share of a processor; a set's load average is at least 1.
    let sched_usage = TIMER_RATE * SCHEDULER_SCALE;
    let mut thread = CACHE.allocate(Thread {
        sleep_link: LinkedListAtomicLink::new(),
        run_link: LinkedListAtomicLink::new(),
        task_link: LinkedListAtomicLink::new(),
        set_link: LinkedListAtomicLink::new(),
        references: AtomicU32::new(2),
        // SAFETY: a task lives while it has threads.
        task: unsafe { UnsafeRef::from_raw(task) },
        state: AtomicU32::new(ThreadState::SUSPENDED.bits()),
        // Set below with the priorities, under the set's and task's tokens.
        suspend_count: AtomicU32::new(0),
        user_stop_count: AtomicU32::new(1),
        priority: 0,
        max_priority: 0,
        sched_pri: AtomicU32::new(0),
        policy: Policies::TIMESHARE,
        cpu_usage: AtomicU32::new(TIMER_RATE),
        sched_usage: AtomicU32::new(sched_usage),
        sched_stamp: AtomicU32::new(sched::scheduler_tick()),
        active: AtomicBool::new(true),
        set: AtomicPtr::new(ptr::null_mut()),
        may_assign: true,
        creation_time: clock::stamp(),
        name: task.name.map(AtomicU8::new),
        // SAFETY: its box, unused until given up below, outlives the thread.
        pcb: unsafe { UnsafeRef::from_raw(ptr::from_mut(&mut *pcb)) },
        self_port: AtomicPtr::new(ptr::null_mut()),
        self_send: AtomicPtr::new(ptr::null_mut()),
        start: OnceLock::new(),
        kernel_stack: OnceLock::new(),
        last_processor: AtomicPtr::new(ptr::null_mut()),
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
    let members = task.members.lock();
    // SAFETY: under the task's token its reference keeps its set alive, and
    // the one taken here does after.
    let mut set = unsafe { &*task.set.load(Ordering::Acquire) };
    set.references.fetch_add(1, Ordering::Relaxed);
    drop(members);
    // The set's token, then the task's, again if the task changed sets; a
    // set out of use gives way to the default set.
    let (mut set_members, mut members) = loop {
        let set_members = set.members.lock();
        let members = task.members.lock();
        // SAFETY: as above; a new set gets its reference before the tokens
        // drop.
        let current = unsafe { &*task.set.load(Ordering::Acquire) };
        let current = if current.active {
            current
        } else {
            &DEFAULT_SET
        };
        if ptr::eq(current, set) {
            break (set_members, members);
        }
        current.references.fetch_add(1, Ordering::Relaxed);
        drop(members);
        drop(set_members);
        // Never the last: a set keeps its own reference until destroyed.
        set.references.fetch_sub(1, Ordering::Relaxed);
        set = current;
    };
    thread.max_priority = members.max_priority.max(set.max_priority);
    thread.priority = members.priority.max(thread.max_priority);
    *thread.sched_pri.get_mut() = computed_priority(thread.priority, sched_usage);
    *thread.suspend_count.get_mut() =
        members.suspend_count + 1 + u32::from(set.empty.load(Ordering::Relaxed));
    // Nothing is published yet; only the set's reference is undone.
    if !members.active {
        drop(members);
        drop(set_members);
        set.references.fetch_sub(1, Ordering::Relaxed);
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
