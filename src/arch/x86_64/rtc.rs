// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The real-time clock in the PC's CMOS: an MC146818 behind two I/O ports.

use mc146818::Bus;
use x86_64::instructions::port::Port;

/// Its index and data ports.
const INDEX: u16 = 0x70;
const DATA: u16 = 0x71;

/// The chip, reached through its index and data ports.
pub struct Cmos;

impl Bus for Cmos {
    fn read(&mut self, register: u8) -> u8 {
        // SAFETY: the CMOS index and data ports; reading a register has no
        // other effect.
        unsafe {
            Port::<u8>::new(INDEX).write(register);
            Port::<u8>::new(DATA).read()
        }
    }

    fn write(&mut self, register: u8, value: u8) {
        // SAFETY: the CMOS index and data ports; writing a register touches
        // only the chip.
        unsafe {
            Port::<u8>::new(INDEX).write(register);
            Port::<u8>::new(DATA).write(value);
        }
    }
}
