// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The high precision event timer: its main counter, which only counts.

use super::acpi;
use crate::println;
use crate::sync::OnceLock;

/// Its registers, by byte offset.
mod register {
    /// Capabilities; the counter's period, in femtoseconds, in the high half.
    pub const CAPABILITIES: usize = 0;
    pub const PERIOD: usize = 4;
    pub const CONFIGURATION: usize = 0x10;
    pub const COUNTER: usize = 0xf0;
}

/// The capability bit of a 64-bit counter.
const WIDE_COUNTER: u32 = 1 << 13;
/// The configuration's enable bit.
const ENABLE: u32 = 1;
/// The configuration's legacy routing bit.
const LEGACY_ROUTE: u32 = 1 << 1;

/// How often the counter ticks, in femtoseconds, once `init` starts it.
pub static PERIOD: OnceLock<u64> = OnceLock::new();

/// Starts the counter from 0, with legacy routing off and no timer
/// interrupt, on the boot CPU.
///
/// # Panics
///
/// If its counter is not 64-bit, or on a second call.
pub fn init() {
    let Some(registers) = acpi::HPET.get() else {
        println!("HPET not available");
        return;
    };
    assert!(
        registers.read(register::CAPABILITIES) & WIDE_COUNTER != 0,
        "hpet: no 64-bit counter"
    );
    let period = registers.read(register::PERIOD);
    println!("HPET ticks every {} nanoseconds", period / 1_000_000);
    let configuration = registers.read(register::CONFIGURATION) & !(LEGACY_ROUTE | ENABLE);
    registers.write(register::CONFIGURATION, configuration);
    registers.write(register::COUNTER, 0);
    registers.write(register::COUNTER + 4, 0);
    registers.write(register::CONFIGURATION, configuration | ENABLE);
    assert!(PERIOD.set(u64::from(period)).is_ok(), "hpet: started twice");
    println!("HPET enabled");
}

/// The counter's value.
///
/// # Panics
///
/// If there is no HPET.
pub fn read() -> u64 {
    let registers = acpi::HPET.get().expect("hpet: none");
    // The high half, then the low, until the high half holds still.
    loop {
        let high = registers.read(register::COUNTER + 4);
        let low = registers.read(register::COUNTER);
        if registers.read(register::COUNTER + 4) == high {
            return u64::from(high) << 32 | u64::from(low);
        }
    }
}
