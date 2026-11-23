// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ops::DerefMut;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU32, Ordering};

use crate::collections::ArrayVec;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::sched::priority::Priority;
use crate::sched::queue::{RUN_QUEUES, RunQueue};
use crate::sched::switch::MAX_STUCK_THREADS;
use crate::smp;
use crate::sync::{CpuLock, CriticalSection, OnceLock};
use crate::thread::thread::{RunQueueAdapter, Thread};

use super::set::ProcessorSet;
use super::state::State;

/// A processor: one per CPU, in that CPU's data.
pub struct Processor {
    /// What it is doing, a `State`.
    pub(super) state: AtomicU8,
    /// How many threads wait on its run queues.
    queued: AtomicU32,
    /// How many of them are not bound to it; changed under their lock.
    pub(super) unbound: AtomicU32,
    /// The ticks left of its thread's quantum.
    quantum: AtomicI32,
    /// Whether its thread runs its first quantum in succession.
    first_quantum: AtomicBool,
    /// The quantum it last gave out, before adjustment.
    last_quantum: AtomicU32,
    /// The processor set it is in, once it comes up.
    pub(super) set: AtomicPtr<ProcessorSet>,
    /// Its runnable threads.
    run_queues: CpuLock<RunQueues>,
    /// Its idle thread, once that runs.
    idle_thread: OnceLock<&'static Thread>,
    /// Its port, for controlling it.
    port: AtomicPtr<Port>,
    /// Its name port, for information about it.
    name_port: AtomicPtr<Port>,
}

/// A processor's runnable threads, bound to it or not.
struct RunQueues {
    /// By priority, each in the order they became runnable.
    lists: [RunQueue; RUN_QUEUES],
    /// How many of them are bound to it.
    bound: u32,
}

impl Processor {
    /// One not yet in the system.
    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(State::Offline.number()),
            queued: AtomicU32::new(0),
            unbound: AtomicU32::new(0),
            quantum: AtomicI32::new(0),
            first_quantum: AtomicBool::new(false),
            last_quantum: AtomicU32::new(0),
            set: AtomicPtr::new(ptr::null_mut()),
            // SAFETY: only `run_queues` locks it, once it checked the CPU.
            run_queues: unsafe {
                CpuLock::new(RunQueues {
                    lists: [const { RunQueue::new(RunQueueAdapter::NEW) }; RUN_QUEUES],
                    bound: 0,
                })
            },
            idle_thread: OnceLock::new(),
            port: AtomicPtr::new(ptr::null_mut()),
            name_port: AtomicPtr::new(ptr::null_mut()),
        }
    }

    /// What it is doing.
    ///
    /// # Panics
    ///
    /// If its state is not one used here.
    pub(crate) fn state(&self) -> State {
        match self.state.load(Ordering::Acquire) {
            0 => State::Offline,
            1 => State::Running,
            2 => State::Idle,
            state => panic!("processor: bad state {state}"),
        }
    }

    /// Sets what it is doing, on its CPU.
    pub(crate) fn set_state(&self, state: State) {
        self.state.store(state.number(), Ordering::Release);
    }

    /// Whether it is running, not offline, as `processor_info` reports it.
    pub fn is_running(&self) -> bool {
        !matches!(self.state(), State::Offline)
    }

    /// Its slot number: the CPU whose processor it is.
    pub fn slot(&self) -> usize {
        (0..crate::config::MAX_CPUS)
            .find(|&cpu| ptr::eq(smp::of(cpu).processor(), self))
            .expect("processor: a processor in no CPU's data")
    }

    /// How many threads wait on its run queues; changed under their lock.
    pub(crate) fn queued(&self) -> u32 {
        self.queued.load(Ordering::Relaxed)
    }

    /// Takes `ticks` off its quantum, on its CPU; returns what is left.
    pub(crate) fn charge_quantum(&self, ticks: u32) -> i32 {
        let left = self.quantum.load(Ordering::Relaxed) - i32::try_from(ticks).unwrap();
        self.quantum.store(left, Ordering::Relaxed);
        left
    }

    /// Sets the ticks left of its quantum, on its CPU.
    pub(crate) fn set_quantum(&self, ticks: i32) {
        self.quantum.store(ticks, Ordering::Relaxed);
    }

    /// Whether its thread runs its first quantum in succession.
    pub(crate) fn is_first_quantum(&self) -> bool {
        self.first_quantum.load(Ordering::Relaxed)
    }

    /// Starts its thread's first quantum in succession, on its CPU.
    pub(crate) fn start_first_quantum(&self) {
        self.first_quantum.store(true, Ordering::Relaxed);
    }

    /// Ends its thread's first quantum in succession, on its CPU.
    pub(crate) fn end_first_quantum(&self) {
        self.first_quantum.store(false, Ordering::Relaxed);
    }

    /// The quantum it last gave out, before adjustment.
    pub(crate) fn last_quantum(&self) -> u32 {
        self.last_quantum.load(Ordering::Relaxed)
    }

    /// Sets the quantum it last gave out, on its CPU.
    pub(crate) fn set_last_quantum(&self, quantum: u32) {
        self.last_quantum.store(quantum, Ordering::Relaxed);
    }

    /// Its run queues, locked on its CPU, or on another before it starts.
    ///
    /// # Panics
    ///
    /// On another CPU once it runs: that comes with SMP.
    fn run_queues(&self) -> impl DerefMut<Target = RunQueues> + '_ {
        // The thread stays on the CPU checked until the lock enters its own
        // section.
        let _section = CriticalSection::enter();
        assert!(
            ptr::eq(self, smp::current().processor()) || matches!(self.state(), State::Offline),
            "run queue: another running CPU's, not yet"
        );
        self.run_queues.lock()
    }

    /// How many threads bound to it wait on its run queues, on its CPU.
    pub(crate) fn bound_queued(&self) -> u32 {
        self.run_queues().bound
    }

    /// Puts `thread` last at its scheduled priority.
    pub(crate) fn enqueue(&self, thread: &'static Thread) {
        let mut queues = self.run_queues();
        let priority = thread.scheduled_priority().index();
        queues.lists[priority].push_back(thread);
        if thread.bound().is_some() {
            queues.bound += 1;
        } else {
            self.unbound.fetch_add(1, Ordering::Relaxed);
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
    }

    /// Takes `thread` off its run queues, on its CPU, as `rem_runq` does;
    /// gives whether it waited there.
    pub(crate) fn remove(&self, thread: &Thread) -> bool {
        let mut queues = self.run_queues();
        let RunQueues { lists, bound } = &mut *queues;
        // Its priority changes only off the run queues. Found by address: a
        // dead list shares its link.
        let mut cursor = lists[thread.scheduled_priority().index()].front_mut();
        while let Some(queued) = cursor.get() {
            if !ptr::eq(queued, thread) {
                cursor.move_next();
                continue;
            }
            cursor.remove();
            if thread.bound().is_some() {
                *bound -= 1;
            } else {
                self.unbound.fetch_sub(1, Ordering::Relaxed);
            }
            self.queued.fetch_sub(1, Ordering::Relaxed);
            return true;
        }
        false
    }

    /// The best priority among the threads waiting on its run queues that
    /// are not bound to it.
    pub(super) fn best_unbound(&self) -> Option<Priority> {
        let queues = self.run_queues();
        queues
            .lists
            .iter()
            .flat_map(RunQueue::iter)
            .find(|thread| thread.bound().is_none())
            .map(Thread::scheduled_priority)
    }

    /// Its first thread bound to it, which `dequeue` takes first, left on
    /// its run queues.
    pub(crate) fn first_bound(&self) -> Option<&'static Thread> {
        let queues = self.run_queues();
        let thread = queues
            .lists
            .iter()
            .flat_map(RunQueue::iter)
            .find(|thread| thread.bound().is_some())?;
        // SAFETY: `enqueue` takes only `&'static Thread`, so each thread on a
        // run queue is one.
        Some(unsafe { &*ptr::from_ref(thread) })
    }

    /// Takes the best of its threads bound to it, or if none the best of
    /// all; the first to become runnable among equals.
    ///
    /// # Panics
    ///
    /// If it counts bound threads but has none.
    pub(crate) fn dequeue(&self) -> Option<&'static Thread> {
        let mut queues = self.run_queues();
        let bound_only = queues.bound > 0;
        let Some(thread) = queues.lists.iter_mut().find_map(|list| {
            let mut cursor = list.front_mut();
            while let Some(thread) = cursor.get() {
                if !bound_only || thread.bound().is_some() {
                    return cursor.remove();
                }
                cursor.move_next();
            }
            None
        }) else {
            assert!(!bound_only, "choose_thread: no bound thread");
            return None;
        };
        if bound_only {
            queues.bound -= 1;
        } else {
            self.unbound.fetch_sub(1, Ordering::Relaxed);
        }
        self.queued.fetch_sub(1, Ordering::Relaxed);
        Some(thread)
    }

    /// Takes its stuck threads off its run queues into `stuck`, best first,
    /// on its CPU; returns whether one found `stuck` full.
    pub(crate) fn take_stuck(
        &self,
        stuck: &mut ArrayVec<&'static Thread, MAX_STUCK_THREADS>,
    ) -> bool {
        let mut queues = self.run_queues();
        let RunQueues { lists, bound } = &mut *queues;
        for list in lists {
            let mut cursor = list.front_mut();
            while let Some(thread) = cursor.get() {
                if !thread.is_stuck() {
                    cursor.move_next();
                    continue;
                }
                if stuck.is_full() {
                    return true;
                }
                let thread = cursor.remove().unwrap();
                if thread.bound().is_some() {
                    *bound -= 1;
                } else {
                    self.unbound.fetch_sub(1, Ordering::Relaxed);
                }
                self.queued.fetch_sub(1, Ordering::Relaxed);
                stuck.push(thread);
            }
        }
        false
    }

    /// The set it is in, once it comes up.
    pub(crate) fn set(&self) -> Option<&'static ProcessorSet> {
        // SAFETY: a set lives while it has processors.
        unsafe { self.set.load(Ordering::Acquire).as_ref() }
    }

    /// Its idle thread, once that runs.
    pub(crate) fn idle_thread(&self) -> Option<&'static Thread> {
        self.idle_thread.get().copied()
    }

    /// Sets its idle thread, once, on its CPU.
    pub(crate) fn set_idle_thread(&self, thread: &'static Thread) -> Result<(), &'static Thread> {
        self.idle_thread.set(thread)
    }

    /// Sets its port, once, at boot; the slot keeps its reference.
    pub(crate) fn set_port(&self, port: Reference<Port>) {
        self.port
            .store(Reference::into_raw(port).as_ptr(), Ordering::Release);
    }

    /// Sets its name port, once, at boot; the slot keeps its reference.
    pub(crate) fn set_name_port(&self, port: Reference<Port>) {
        self.name_port
            .store(Reference::into_raw(port).as_ptr(), Ordering::Release);
    }
}
