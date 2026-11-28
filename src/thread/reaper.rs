// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;

use crate::ast::Ast;
use crate::config::MAX_CPUS;
use crate::reference::Reference;
use crate::sched::{self, event::Event};
use crate::smp;
use crate::sync::CriticalSection;

use super::state::ThreadState;

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
        sched::switch::block();
        return;
    }
    thread.terminate_ipc();
    thread.hold();
    // It stays on this CPU until it blocks: the list it joins and the
    // sleep queue woken below are this CPU's.
    let section = CriticalSection::enter();
    // SAFETY: the list's reference keeps the thread alive, and off the run
    // queues only this list writes its run link, under the lock.
    unsafe {
        smp::current()
            .dead_threads()
            .lock()
            .push_back_ptr(NonNull::from(thread));
    }
    thread.insert_state(ThreadState::HALTED);
    drop(section);
    sched::wait::wakeup(Event::of(&REAPER));
    sched::switch::block();
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
            (0..MAX_CPUS).find_map(|cpu| smp::of(cpu).dead_threads().lock().pop_front_ptr())
        {
            drop(section);
            // SAFETY: a dead list holds the reference of its thread's being
            // alive, given up here.
            let thread = unsafe { Reference::from_raw(thread) };
            thread.wait_until_stopped();
            drop(thread);
            section = CriticalSection::enter();
        }
        sched::wait::assert_wait(Some(Event::of(&REAPER)));
        sched::switch::block();
        drop(section);
    }
}
