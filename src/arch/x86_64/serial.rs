// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! COM1 serial port.

use core::fmt;

use x86_64::instructions::port::Port;

const COM1: u16 = 0x3f8;

pub fn init() {
    // SAFETY: COM1 registers.
    unsafe {
        Port::<u8>::new(COM1 + 1).write(0x00); // disable interrupts
        Port::<u8>::new(COM1 + 3).write(0x80); // enable DLAB to set the baud rate divisor
        Port::<u8>::new(COM1).write(0x01); // divisor low byte: 115200 baud
        Port::<u8>::new(COM1 + 1).write(0x00); // divisor high byte
        Port::<u8>::new(COM1 + 3).write(0x03); // 8 bits, no parity, one stop bit; DLAB off
        Port::<u8>::new(COM1 + 2).write(0xc7); // enable + clear FIFOs
        Port::<u8>::new(COM1 + 4).write(0x0b); // DTR, RTS, OUT2
    }
}

pub struct Serial;

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &byte in s.as_bytes() {
            // SAFETY: COM1 data register.
            unsafe { Port::new(COM1).write(byte) };
        }
        Ok(())
    }
}
