// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! 8259A interrupt controllers.

use x86_64::instructions::port::Port;

const MASTER_DATA: u16 = 0x21;
const SLAVE_DATA: u16 = 0xa1;

pub fn disable() {
    // SAFETY: 8259A mask registers.
    unsafe {
        Port::<u8>::new(SLAVE_DATA).write(0xff);
        Port::<u8>::new(MASTER_DATA).write(0xff);
    }
}
