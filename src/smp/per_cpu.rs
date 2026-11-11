// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data: the machine-independent part, and the trait through which
//! it reaches the machine's part.

use core::ops::DerefMut;
use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering, compiler_fence};

use intrusive_collections::LinkedList;

use crate::arch;
use crate::ast::Ast;
use crate::clock::Wheel;
use crate::processor::Processor;
use crate::sched::{SLEEP_QUEUES, SleepQueue};
use crate::sync::{CpuLock, CriticalSection, Spinlock};
use crate::thread::{RunQueueAdapter, SleepQueueAdapter, Thread};

/// One CPU's machine-independent data, held in the machine's part.
pub struct PerCpu {
    id: usize,
    /// How deep its interrupt handlers nest.
    interrupt_nesting: AtomicU32,
    /// How deep its critical sections nest before its first thread runs.
    boot_nesting: AtomicU32,
    /// The count of the thread it runs, once it runs one: a thread keeps
    /// how deep its own critical sections nest.
    nesting: AtomicPtr<AtomicU32>,
    /// How many spinlocks it holds.
    spinlocks: AtomicU32,
    /// How many tokens its thread holds.
    tokens: AtomicU32,
    /// Its sleep queues, by event hash; only it locks them.
    sleep_queues: CpuLock<[SleepQueue; SLEEP_QUEUES]>,
    /// Its processor.
    processor: Processor,
    /// The thread it runs; null before its first.
    active_thread: AtomicPtr<Thread>,
    /// The threads that ended on it, waiting for the reaper, linked as on
    /// a run queue.
    dead_threads: Spinlock<LinkedList<RunQueueAdapter>>,
    /// Its timeouts; only it runs them.
    timeouts: Wheel,
    /// The ASTs pending on it, a bit per reason.
    pending_asts: AtomicU32,
}

impl PerCpu {
    /// CPU `id`'s, before it starts.
    #[expect(
        clippy::large_stack_arrays,
        reason = "only PER_CPU's initializer calls it, at compile time"
    )]
    pub const fn new(id: usize) -> Self {
        Self {
            id,
            interrupt_nesting: AtomicU32::new(0),
            boot_nesting: AtomicU32::new(0),
            nesting: AtomicPtr::new(ptr::null_mut()),
            spinlocks: AtomicU32::new(0),
            tokens: AtomicU32::new(0),
            // SAFETY: only `sleep_queues` locks it, once it checked the CPU.
            sleep_queues: unsafe {
                CpuLock::new([const { SleepQueue::new(SleepQueueAdapter::NEW) }; SLEEP_QUEUES])
            },
            processor: Processor::new(),
            active_thread: AtomicPtr::new(ptr::null_mut()),
            dead_threads: Spinlock::new(LinkedList::new(RunQueueAdapter::NEW)),
            timeouts: Wheel::new(),
            pending_asts: AtomicU32::new(0),
        }
    }

    /// Its number.
    pub const fn id(&self) -> usize {
        self.id
    }

    /// Its processor.
    pub const fn processor(&self) -> &Processor {
        &self.processor
    }

    /// Its timeouts.
    pub const fn timeouts(&self) -> &Wheel {
        &self.timeouts
    }

    /// The threads that ended on it, waiting for the reaper.
    pub const fn dead_threads(&self) -> &Spinlock<LinkedList<RunQueueAdapter>> {
        &self.dead_threads
    }

    /// Its sleep queues, locked.
    ///
    /// # Panics
    ///
    /// If it is not the current CPU.
    pub fn sleep_queues(&self) -> impl DerefMut<Target = [SleepQueue; SLEEP_QUEUES]> + '_ {
        // The thread stays on the CPU checked until the lock enters its own
        // section.
        let _section = CriticalSection::enter();
        assert!(
            ptr::eq(self, current()),
            "sleep queue: another CPU's, not yet"
        );
        self.sleep_queues.lock()
    }

    /// The thread it runs, once it runs one.
    pub fn active_thread(&self) -> Option<&'static Thread> {
        // SAFETY: a thread lives while a CPU runs it.
        unsafe { self.active_thread.load(Ordering::Acquire).as_ref() }
    }

    /// Makes `thread` the one it runs, as it switches to it.
    pub fn set_active_thread(&self, thread: &'static Thread) {
        self.active_thread
            .store(ptr::from_ref(thread).cast_mut(), Ordering::Release);
    }

    /// How deep the critical sections of what it runs nest: its thread's,
    /// or its own before its first thread.
    fn nesting(&self) -> &AtomicU32 {
        // SAFETY: a thread lives while a CPU runs it.
        unsafe { self.nesting.load(Ordering::Relaxed).as_ref() }.unwrap_or(&self.boot_nesting)
    }

    /// Makes `count` its nesting, as it switches to the thread that keeps
    /// it, inside a critical section.
    pub fn switch_nesting(&self, count: &'static AtomicU32) {
        self.nesting
            .store(ptr::from_ref(count).cast_mut(), Ordering::Relaxed);
    }

    /// Notes a spinlock taken on this CPU, inside a critical section.
    pub fn take_spinlock(&self) {
        raise(&self.spinlocks);
    }

    /// Notes a spinlock released on this CPU.
    pub fn release_spinlock(&self) {
        lower(&self.spinlocks);
    }

    /// How many spinlocks this CPU holds.
    pub fn spinlocks(&self) -> u32 {
        self.spinlocks.load(Ordering::Relaxed)
    }

    /// Notes a token taken on this CPU.
    pub fn take_token(&self) {
        raise(&self.tokens);
    }

    /// Notes a token released on this CPU.
    pub fn release_token(&self) {
        lower(&self.tokens);
    }

    /// How many tokens this CPU's thread holds.
    pub fn tokens(&self) -> u32 {
        self.tokens.load(Ordering::Relaxed)
    }

    /// Posts `asts` on it.
    pub fn add_asts(&self, asts: Ast) {
        self.pending_asts.fetch_or(asts.bits(), Ordering::Relaxed);
    }

    /// Clears `asts` on it.
    pub fn remove_asts(&self, asts: Ast) {
        self.pending_asts.fetch_and(!asts.bits(), Ordering::Relaxed);
    }

    /// The ASTs pending on it.
    pub fn asts(&self) -> Ast {
        Ast::from_bits_retain(self.pending_asts.load(Ordering::Relaxed))
    }

    /// Takes the ASTs pending on it, which it then has none of.
    pub fn take_asts(&self) -> Ast {
        Ast::from_bits_retain(self.pending_asts.swap(0, Ordering::Relaxed))
    }

    /// Enters an interrupt handler on this CPU; dropping the guard leaves it.
    pub fn enter_interrupt(&self) -> InterruptGuard<'_> {
        raise(&self.interrupt_nesting);
        InterruptGuard(self)
    }

    /// Enters a critical section on this CPU.
    pub fn enter_critical(&self) {
        raise(self.nesting());
        compiler_fence(Ordering::SeqCst);
    }

    /// Leaves a critical section on this CPU, returning whether it was the
    /// outermost.
    pub fn leave_critical(&self) -> bool {
        compiler_fence(Ordering::SeqCst);
        let outermost = lower(self.nesting()) == 0;
        compiler_fence(Ordering::SeqCst);
        outermost
    }

    /// Whether this CPU is in a critical section.
    pub fn in_critical_section(&self) -> bool {
        self.nesting().load(Ordering::Relaxed) != 0
    }

    /// Whether this CPU is in an interrupt handler.
    pub fn in_interrupt(&self) -> bool {
        self.interrupt_nesting.load(Ordering::Relaxed) != 0
    }
}

/// An interrupt handler running on its CPU; dropping it leaves the handler.
pub struct InterruptGuard<'a>(&'a PerCpu);

impl Drop for InterruptGuard<'_> {
    fn drop(&mut self) {
        lower(&self.0.interrupt_nesting);
    }
}

/// Raises a count only its own CPU changes; an interrupt between the load
/// and the store leaves it as it found it.
fn raise(count: &AtomicU32) {
    count.store(count.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
}

/// Lowers such a count, returning the new value.
fn lower(count: &AtomicU32) -> u32 {
    let lowered = count.load(Ordering::Relaxed) - 1;
    count.store(lowered, Ordering::Relaxed);
    lowered
}

/// The machine's part of each CPU's data, holding a `PerCpu`; the
/// machine-independent side reaches it only through this.
pub trait MachinePerCpu: Sync + 'static {
    /// The current CPU's.
    fn current() -> &'static Self;
    /// CPU `cpu`'s.
    fn of(cpu: usize) -> &'static Self;

    /// Its machine-independent part.
    fn common(&self) -> &PerCpu;

    /// Runs the interrupts its critical sections held back.
    fn run_pending_interrupts(&self);
}

/// The current CPU's machine-independent data.
pub fn current() -> &'static PerCpu {
    arch::PerCpu::current().common()
}

/// CPU `cpu`'s machine-independent data.
pub fn of(cpu: usize) -> &'static PerCpu {
    arch::PerCpu::of(cpu).common()
}
