// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! I/O ports.

use core::arch::asm;

/// An I/O port.
pub struct Port(pub u16);

impl Port {
    /// Reads a byte.
    ///
    /// # Safety
    ///
    /// Port reads can have hardware side effects.
    #[expect(dead_code, reason = "no reader yet")]
    pub unsafe fn read_u8(&self) -> u8 {
        let value;
        // SAFETY: guaranteed by the caller.
        unsafe {
            asm!("in %dx, %al", out("al") value, in("dx") self.0, options(nomem, nostack, preserves_flags, att_syntax));
        }
        value
    }

    /// Writes a byte.
    ///
    /// # Safety
    ///
    /// Port writes can have any hardware side effect, including DMA.
    pub unsafe fn write_u8(&self, value: u8) {
        // SAFETY: guaranteed by the caller.
        unsafe {
            asm!("out %al, %dx", in("al") value, in("dx") self.0, options(nomem, nostack, preserves_flags, att_syntax));
        }
    }
}
