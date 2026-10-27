// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! COM1 serial port.

use core::fmt;

use super::port::Port;

const COM1: u16 = 0x3f8;

pub fn init() {
    // SAFETY: COM1 registers.
    unsafe {
        Port(COM1 + 1).write_u8(0x00); // disable interrupts
        Port(COM1 + 3).write_u8(0x80); // enable DLAB to set the baud rate divisor
        Port(COM1).write_u8(0x03); // divisor low byte: 38400 baud
        Port(COM1 + 1).write_u8(0x00); // divisor high byte
        Port(COM1 + 3).write_u8(0x03); // 8 bits, no parity, one stop bit; DLAB off
        Port(COM1 + 2).write_u8(0xc7); // enable + clear FIFOs
        Port(COM1 + 4).write_u8(0x0b); // RTS/DSR set
    }
}

pub struct Serial;

impl fmt::Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for &byte in s.as_bytes() {
            // SAFETY: COM1 data register.
            unsafe { Port(COM1).write_u8(byte) };
        }
        Ok(())
    }
}
