// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Critical sections: they hold the current CPU's interrupts back.

use core::arch::asm;
use core::mem::offset_of;

use x86_64::instructions::interrupts;

use super::interrupt;
use super::per_cpu::PerCpu;

/// Enters a critical section on the current CPU; its interrupts wait for
/// the outermost `crit_exit`.
pub fn crit_enter() {
    // SAFETY: the per-CPU data is set up, and its count is a `u32`.
    unsafe {
        asm!(
            "incl %gs:{nesting}",
            nesting = const offset_of!(PerCpu, critical_nesting),
            options(att_syntax, nostack),
        );
    }
}

/// Leaves a critical section; leaving the outermost runs the interrupts
/// held back.
pub fn crit_exit() {
    if !leave() {
        return;
    }
    while let Some(vector) = interrupts::without_interrupts(take_pending) {
        crit_enter();
        interrupt::handle(vector, None);
        leave();
    }
}

/// Holds `vector` back if the current CPU is in a critical section,
/// returning whether it did.
pub fn hold(vector: u64) -> bool {
    let nesting: u32;
    // SAFETY: as in `crit_enter`.
    unsafe {
        asm!(
            "movl %gs:{offset}, {nesting:e}",
            offset = const offset_of!(PerCpu, critical_nesting),
            nesting = out(reg) nesting,
            options(att_syntax, nostack, preserves_flags, readonly),
        );
    }
    if nesting == 0 {
        return false;
    }
    // SAFETY: the per-CPU data is set up, and its pending set has a bit
    // for each vector.
    unsafe {
        asm!(
            "btsq {vector}, %gs:{pending}",
            pending = const offset_of!(PerCpu, pending_interrupts),
            vector = in(reg) vector,
            options(att_syntax, nostack),
        );
    }
    true
}

/// Lowers the current CPU's count, returning whether it reached zero.
fn leave() -> bool {
    let outermost: u8;
    // SAFETY: as in `crit_enter`.
    unsafe {
        asm!(
            "decl %gs:{nesting}",
            "setz {outermost}",
            nesting = const offset_of!(PerCpu, critical_nesting),
            outermost = out(reg_byte) outermost,
            options(att_syntax, nostack),
        );
    }
    outermost != 0
}

/// Takes the current CPU's lowest pending vector, if any; with interrupts
/// off.
fn take_pending() -> Option<u64> {
    // The pending set's four words.
    for word in 0..4_u64 {
        let bits: u64;
        // SAFETY: as in `hold`; `word` is in the set.
        unsafe {
            asm!(
                "movq %gs:{pending}(, {word}, 8), {bits}",
                pending = const offset_of!(PerCpu, pending_interrupts),
                word = in(reg) word,
                bits = out(reg) bits,
                options(att_syntax, nostack, preserves_flags, readonly),
            );
        }
        if bits != 0 {
            let vector = word * 64 + u64::from(bits.trailing_zeros());
            // SAFETY: as in `hold`.
            unsafe {
                asm!(
                    "btrq {vector}, %gs:{pending}",
                    pending = const offset_of!(PerCpu, pending_interrupts),
                    vector = in(reg) vector,
                    options(att_syntax, nostack),
                );
            }
            return Some(vector);
        }
    }
    None
}
