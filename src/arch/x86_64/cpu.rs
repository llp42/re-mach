// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Privileged processor operations for the bootstrap CPU.

use core::arch::asm;

/// Stops this CPU with maskable interrupts disabled; never returns.
pub fn halt() -> ! {
    loop {
        // SAFETY: kernel execution is at privilege level zero. No interrupt
        // handler or scheduler is installed yet. NMI wakeups reenter the loop.
        unsafe { asm!("cli", "hlt", options(nomem, nostack, att_syntax)) };
    }
}
