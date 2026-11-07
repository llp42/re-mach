// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Asynchronous system traps: what a CPU does before it goes back to user
//! mode.

use core::ptr;
use core::sync::atomic::Ordering;

use crate::processor::State;
use crate::sched::{self, Policies, Policy};
use crate::smp;
use crate::sync::CriticalSection;
use crate::thread::{Thread, ThreadState};

bitflags::bitflags! {
    /// Why a CPU takes an AST, as Mach numbers them; only those used here.
    #[derive(Clone, Copy)]
    pub struct Ast: u32 {
        /// Its thread should halt.
        const HALT = 0x1;
        /// Its thread should end.
        const TERMINATE = 0x2;
        /// Its thread should give up the processor.
        const BLOCK = 0x4;
    }
}

/// Takes the current CPU's ASTs, with interrupts on: unless it runs its
/// idle thread, the thread gives the processor up on `BLOCK`.
///
/// # Panics
///
/// If the thread should halt, or no `BLOCK` is pending, where GNU Mach
/// tests `csw_needed`: not yet.
pub fn taken() {
    let cpu = smp::current();
    let thread = cpu.active_thread().expect("ast_taken: no current thread");
    let section = CriticalSection::enter();
    let reasons = cpu.take_asts();
    drop(section);
    if cpu
        .processor()
        .idle_thread
        .get()
        .is_some_and(|idle| ptr::eq(*idle, thread))
    {
        return;
    }
    assert!(
        !Ast::from_bits_retain(thread.asts.load(Ordering::Relaxed))
            .intersects(Ast::HALT | Ast::TERMINATE),
        "thread_halt_self: not yet"
    );
    assert!(
        reasons.contains(Ast::BLOCK),
        "ast_taken: csw_needed, not yet"
    );
    sched::block();
}

/// Checks whether `thread`, running on the current CPU, should give the
/// processor up, and asks for an AST if so; a CPU already taking one
/// looks no further.
pub fn check(thread: &Thread) {
    let cpu = smp::current();
    let processor = cpu.processor();
    match processor.state() {
        State::Offline | State::Idle => {}
        State::Running => {
            cpu.add_asts(Ast::from_bits_retain(thread.asts.load(Ordering::Relaxed)));
            if !cpu.asts().is_empty() {
                return;
            }
            if thread.state.load(Ordering::Relaxed) & ThreadState::SUSPENDED.bits() != 0
                || processor.bound_queued() != 0
            {
                cpu.add_asts(Ast::BLOCK);
                return;
            }
            match processor.set() {
                // A fixed-priority thread uses all of a new quantum.
                Some(set) if set.policies.contains(Policies::FIXED_PRIORITY) => {
                    if matches!(thread.policy, Policy::FixedPriority) {
                        processor.first_quantum.store(true, Ordering::Relaxed);
                    }
                }
                Some(set)
                    if !processor.first_quantum.load(Ordering::Relaxed) && set.queued() > 0 =>
                {
                    panic!("ast_check: the set's run queue, not yet")
                }
                _ => {}
            }
        }
    }
}
