// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Early COM1 output for the initial PC platform, before concurrent execution.

use core::arch::asm;

const COM1: u16 = 0x3f8;

/// Requires exclusive bootstrap use of the PC's COM1 UART at privilege zero.
pub fn init() {
    write_register(1, 0x00);
    write_register(3, 0x80); // Divisor latch access.
    write_register(0, 0x03); // 38400 baud.
    write_register(1, 0x00);
    write_register(3, 0x03); // Eight data bits, one stop bit, no parity.
    write_register(2, 0xc7); // Enable and clear FIFOs.
    write_register(4, 0x0b);
}

/// Requires `init`; concurrent callers are not supported during early boot.
pub fn write(bytes: &[u8]) {
    for &byte in bytes {
        // Bound polling so an absent UART cannot prevent a fatal halt.
        for _ in 0..100_000 {
            if read_register(5) & 0x20 != 0 {
                write_register(0, byte);
                break;
            }
            core::hint::spin_loop();
        }
    }
}

fn write_register(offset: u16, value: u8) {
    debug_assert!(offset < 8);
    // SAFETY: private callers use only COM1 registers during privileged,
    // single-CPU bootstrap. The instruction does not access Rust memory.
    unsafe {
        asm!("out %al, %dx", in("al") value, in("dx") COM1 + offset,
            options(nomem, nostack, preserves_flags, att_syntax));
    }
}

fn read_register(offset: u16) -> u8 {
    debug_assert!(offset < 8);
    let value;
    // SAFETY: same private, privileged COM1 access as write_register.
    unsafe {
        asm!("in %dx, %al", in("dx") COM1 + offset, out("al") value,
            options(nomem, nostack, preserves_flags, att_syntax));
    }
    value
}
