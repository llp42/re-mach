// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The two 8259 interrupt controllers (dfly's ICU).

use super::port::Port;

/// Master command port; its data port is `MASTER + 1`.
const MASTER: u16 = 0x20;
/// Slave command port; its data port is `SLAVE + 1`.
const SLAVE: u16 = 0xa0;

/// Moves IRQs 0-15 to vectors 0x20-0x2f, past the exceptions, and masks
/// them all, as dfly's `icu_init` does.
pub fn init() {
    // SAFETY: 8259 registers; interrupts are off.
    unsafe {
        Port(MASTER).write_u8(0x11); // ICW1: initialize, cascade, ICW4 follows
        Port(MASTER + 1).write_u8(0x20); // ICW2: IRQs 0-7 at vectors 0x20-0x27
        Port(MASTER + 1).write_u8(0x04); // ICW3: slave on IRQ 2
        Port(MASTER + 1).write_u8(0x01); // ICW4: 8086 mode
        Port(MASTER + 1).write_u8(0xff); // mask all
        Port(SLAVE).write_u8(0x11); // ICW1: initialize, cascade, ICW4 follows
        Port(SLAVE + 1).write_u8(0x28); // ICW2: IRQs 8-15 at vectors 0x28-0x2f
        Port(SLAVE + 1).write_u8(0x02); // ICW3: wired to the master's IRQ 2
        Port(SLAVE + 1).write_u8(0x01); // ICW4: 8086 mode
        Port(SLAVE + 1).write_u8(0xff); // mask all
    }
}
