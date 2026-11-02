// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The task state segment: the CPU's stacks for traps.

use core::cell::UnsafeCell;

use x86_64::VirtAddr;
use x86_64::instructions::tables::load_tss;

use super::{gdt, per_cpu};

/// Each stack's size: two pages.
const STACK_SIZE: usize = 8 * 1024;

#[repr(C, align(16))]
struct Stack(UnsafeCell<[u8; STACK_SIZE]>);

// SAFETY: only the boot CPU uses them, through `%rsp`.
unsafe impl Sync for Stack {}

impl Stack {
    /// Past its last byte, where the CPU starts pushing.
    fn top(&self) -> VirtAddr {
        VirtAddr::from_ptr(self.0.get().cast::<u8>().wrapping_add(STACK_SIZE))
    }
}

/// Ring 0's stack for traps from user mode, until threads have their own.
static PRIVILEGE_STACK: Stack = Stack(UnsafeCell::new([0; STACK_SIZE]));

/// IST 1: the double fault's and the NMI's.
static DOUBLE_FAULT_STACK: Stack = Stack(UnsafeCell::new([0; STACK_SIZE]));

/// IST 2: the debug exception's.
static DEBUG_STACK: Stack = Stack(UnsafeCell::new([0; STACK_SIZE]));

/// Fills the boot CPU's TSS with its stacks and loads it; user mode gets no
/// I/O ports.
///
/// # Safety
///
/// Runs once, on the boot CPU, after `gdt::init` and `per_cpu::init`.
pub unsafe fn init() {
    let tss = per_cpu::tss();
    // SAFETY: guaranteed by the caller; the CPU has not loaded it yet.
    unsafe {
        (*tss).privilege_stack_table[0] = PRIVILEGE_STACK.top();
        (*tss).interrupt_stack_table[0] = DOUBLE_FAULT_STACK.top();
        (*tss).interrupt_stack_table[1] = DEBUG_STACK.top();
    }
    // SAFETY: the TSS is static and filled, and the selector names its
    // descriptor.
    unsafe { load_tss(gdt::store_tss(tss)) };
}
