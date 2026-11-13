// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The task state segment: the CPU's stacks for traps.

use x86_64::VirtAddr;
use x86_64::instructions::tables::load_tss;

use super::gdt;
use super::per_cpu::PerCpu;
use super::stack::Stack;

/// Each stack's size: two pages.
const STACK_SIZE: usize = 8 * 1024;

/// Ring 0's stack for traps from user mode, until threads have their own.
static PRIVILEGE_STACK: Stack<STACK_SIZE> = Stack::new();

/// The boot CPU's IST 1: the double fault's and the NMI's.
pub static DOUBLE_FAULT_STACK: Stack<STACK_SIZE> = Stack::new();

/// The boot CPU's IST 2: the debug exception's.
pub static DEBUG_STACK: Stack<STACK_SIZE> = Stack::new();

/// Fills `cpu`'s TSS with its stacks, IST 1 ending at `double_fault` and
/// IST 2 at `debug`, and loads it; user mode gets no I/O ports.
///
/// # Safety
///
/// Runs once, on `cpu`, after `gdt::init` with its table. `double_fault`
/// and `debug` each end a stack only `cpu`'s traps use, valid for as long
/// as `cpu` runs.
pub unsafe fn init(cpu: &PerCpu, double_fault: VirtAddr, debug: VirtAddr) {
    let tss = cpu.tss();
    let mut filled = tss.get();
    filled.privilege_stack_table[0] = PRIVILEGE_STACK.top();
    filled.interrupt_stack_table[0] = double_fault;
    filled.interrupt_stack_table[1] = debug;
    tss.set(filled);
    // SAFETY: the TSS stays in its per-CPU data, filled with stacks the
    // caller keeps valid, and the selector names its descriptor.
    unsafe { load_tss(gdt::store_tss(cpu.gdt(), tss.as_ptr())) };
}
