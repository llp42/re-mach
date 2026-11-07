// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Resetting the machine.

use core::arch::asm;
use core::hint;

use x86_64::VirtAddr;
use x86_64::instructions::port::Port;
use x86_64::instructions::tables::lidt;
use x86_64::structures::DescriptorTablePointer;

use super::counter;

/// The 8042 keyboard controller's command port.
const COMMAND_PORT: u16 = 0x64;

/// The 8042 command that pulses the CPU's reset line.
const PULSE_RESET: u8 = 0xfe;

/// How long the pulse gets to reset the machine, in nanoseconds.
const PULSE_WAIT: u64 = 1_000_000_000;

/// Resets the machine: the 8042's reset pulse, then, if the machine still
/// runs a second later, a triple fault.
pub fn reset() -> ! {
    // SAFETY: the 8042's command port; the reset ends everything.
    unsafe { Port::<u8>::new(COMMAND_PORT).write(PULSE_RESET) };
    let deadline = counter::nanoseconds() + PULSE_WAIT;
    while counter::nanoseconds() < deadline {
        hint::spin_loop();
    }
    // With no vector, the exception faults twice more: a triple fault.
    let empty = DescriptorTablePointer {
        limit: 0,
        base: VirtAddr::zero(),
    };
    // SAFETY: nothing runs after the reset.
    unsafe {
        lidt(&empty);
        asm!("ud2", options(att_syntax, noreturn));
    }
}
