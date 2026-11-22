// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem;
use core::ptr;
use core::sync::atomic::Ordering;

use crate::processor::Processor;
use crate::smp;
use crate::sync::CriticalSection;
use crate::vm::phys_map::MachinePhysMap as _;

use super::machine::MachineThread as _;
use super::state::ThreadState;
use super::thread::Thread;

impl Thread {
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
    pub(crate) unsafe fn switch(&'static self, next: &'static Self) -> &'static Self {
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
    pub(crate) fn bound(&self) -> Option<&'static Processor> {
        // SAFETY: processors live for good.
        unsafe { self.bound.load(Ordering::Acquire).as_ref() }
    }

    /// The processor it last ran on, if it has run.
    pub(crate) fn last_processor(&self) -> Option<&'static Processor> {
        // SAFETY: processors live for good.
        unsafe { self.last_processor.load(Ordering::Acquire).as_ref() }
    }

    /// Marks it running, as a thread that is about to be switched to.
    pub fn mark_running(&self) {
        self.state
            .fetch_or(ThreadState::RUN.bits(), Ordering::Relaxed);
    }
}
