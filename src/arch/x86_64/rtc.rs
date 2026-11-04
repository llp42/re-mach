// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The real-time clock in the PC's CMOS: an MC146818, read as it is set.

use x86_64::instructions::port::Port;

/// Its index and data ports.
const INDEX: u16 = 0x70;
const DATA: u16 = 0x71;

/// Its registers.
mod register {
    pub const SECONDS: u8 = 0;
    pub const MINUTES: u8 = 2;
    pub const HOURS: u8 = 4;
    pub const DAY: u8 = 7;
    pub const MONTH: u8 = 8;
    pub const YEAR: u8 = 9;
    pub const STATUS_A: u8 = 0xa;
    pub const STATUS_B: u8 = 0xb;
    pub const STATUS_D: u8 = 0xd;
}

/// Status A's bit set while the clock updates.
const UPDATING: u8 = 0x80;
/// Status B's bit for binary values, not BCD.
const BINARY: u8 = 4;
/// Status B's bit for 24-hour mode.
const HOURS_24: u8 = 2;
/// Status D's bit set while the clock's battery holds.
const VALID: u8 = 0x80;
/// The hours' bit for the afternoon, in 12-hour mode.
const AFTERNOON: u8 = 0x80;

/// A date and time, as the clock keeps them.
pub struct DateTime {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// Reads `register`.
fn read_register(register: u8) -> u8 {
    // SAFETY: the CMOS index and data ports; reading a clock register has
    // no other effect.
    unsafe {
        Port::<u8>::new(INDEX).write(register);
        Port::<u8>::new(DATA).read()
    }
}

/// Reads the date and time, on the boot CPU, once no update is under way;
/// none if the clock's battery failed. A two-digit year is 1970 to 2069.
pub fn read() -> Option<DateTime> {
    if read_register(register::STATUS_D) & VALID == 0 {
        return None;
    }
    while read_register(register::STATUS_A) & UPDATING != 0 {}
    let status = read_register(register::STATUS_B);
    let value = |raw: u8| {
        u32::from(if status & BINARY != 0 {
            raw
        } else {
            (raw >> 4) * 10 + (raw & 0xf)
        })
    };
    let raw_hour = read_register(register::HOURS);
    let mut hour = value(raw_hour & !AFTERNOON);
    if status & HOURS_24 == 0 {
        hour = hour % 12 + if raw_hour & AFTERNOON != 0 { 12 } else { 0 };
    }
    let year = value(read_register(register::YEAR));
    Some(DateTime {
        year: year + if year < 70 { 2000 } else { 1900 },
        month: value(read_register(register::MONTH)),
        day: value(read_register(register::DAY)),
        hour,
        minute: value(read_register(register::MINUTES)),
        second: value(read_register(register::SECONDS)),
    })
}

impl DateTime {
    /// Seconds since 1970, taking it as UTC.
    pub fn seconds(&self) -> i64 {
        const MONTH_DAYS: [u32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        let leap = |year: u32| {
            year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
        };
        let month = usize::try_from(self.month.clamp(1, 12)).unwrap();
        let days = (1970..self.year)
            .map(|year| if leap(year) { 366 } else { 365 })
            .sum::<u32>()
            + MONTH_DAYS[..month - 1].iter().sum::<u32>()
            + u32::from(leap(self.year) && month > 2)
            + self.day.saturating_sub(1);
        i64::from(days) * 86400
            + i64::from(self.hour) * 3600
            + i64::from(self.minute) * 60
            + i64::from(self.second)
    }
}
