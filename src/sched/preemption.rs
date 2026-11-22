// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr;

use crate::ast::Ast;
use crate::smp;
use crate::sync::CriticalSection;
use crate::thread::thread::Thread;

/// Whether a thread made runnable may preempt the current one.
#[derive(Clone, Copy)]
pub enum Preemption {
    /// It may, if it is better.
    Allowed,
    /// It may not.
    Forbidden,
}

/// Makes `thread` runnable: it goes on the run queue of the processor it
/// is bound to, which then switches if it is the current CPU's, or else on
/// the current CPU's, which switches if `preemption` allows it and `thread`
/// is better than the one it runs.
///
/// # Panics
///
/// If `thread` is bound to another running CPU, or is not bound and in
/// another set than the current CPU's: those come with SMP.
pub fn make_runnable(thread: &'static Thread, preemption: Preemption) {
    let _section = CriticalSection::enter();
    thread.catch_up();
    let cpu = smp::current();
    let processor = cpu.processor();
    if let Some(bound) = thread.bound() {
        bound.enqueue(thread);
        // Another CPU's is off line, so it takes no AST check.
        if ptr::eq(bound, processor) {
            cpu.add_asts(Ast::BLOCK);
        }
        return;
    }
    assert!(
        processor.set().is_some_and(|set| thread.is_in(set)),
        "thread_setrun: another set's run queue, not yet"
    );
    processor.enqueue(thread);
    if matches!(preemption, Preemption::Allowed) {
        let current = cpu
            .active_thread()
            .expect("thread_setrun: no current thread");
        if thread
            .scheduled_priority()
            .outranks(current.scheduled_priority())
        {
            processor.end_first_quantum();
            cpu.add_asts(Ast::BLOCK);
        }
    }
}
