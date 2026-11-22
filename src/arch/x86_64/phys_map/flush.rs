// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::arch::asm;

use x86_64::instructions::tlb;

/// Writes back and invalidates the caches, then flushes the TLB.
pub(super) fn flush_caches() {
    // SAFETY: only writes back and drops cached lines.
    unsafe { asm!("wbinvd", options(att_syntax, nostack, preserves_flags)) };
    tlb::flush_all();
}
