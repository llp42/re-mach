// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Asynchronous system traps: what a CPU does before it goes back to user
//! mode.

use core::sync::atomic::Ordering;

use super::processor::State;
use super::sched::Policies;
use super::thread::{Thread, ThreadState};
use crate::smp;

bitflags::bitflags! {
    /// Why a CPU takes an AST, as Mach numbers them; only those used here.
    #[derive(Clone, Copy)]
    pub struct Ast: u32 {
        /// Its thread should give up the processor.
        const BLOCK = 0x4;
    }
}

/// Checks whether `thread`, running on the current CPU, should give the
/// processor up, and asks for an AST if so; a CPU already taking one
/// looks no further.
pub fn check(thread: &Thread) {
    let cpu = smp::current();
    let processor = cpu.processor();
    match processor.state() {
        State::Offline => {}
        State::Running => {
            cpu.add_asts(Ast::from_bits_retain(thread.asts.load(Ordering::Relaxed)));
            if !cpu.asts().is_empty() {
                return;
            }
            if thread.state.load(Ordering::Relaxed) & ThreadState::SUSPENDED.bits() != 0
                || processor.queued.load(Ordering::Relaxed) != 0
            {
                cpu.add_asts(Ast::BLOCK);
                return;
            }
            // A fixed-priority thread uses all of a new quantum.
            if processor
                .set()
                .is_some_and(|set| set.policies.contains(Policies::FIXED_PRIORITY))
                && thread.policy == Policies::FIXED_PRIORITY
            {
                processor.first_quantum.store(true, Ordering::Relaxed);
            }
        }
    }
}
