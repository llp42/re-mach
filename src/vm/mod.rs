// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Virtual memory.

pub mod object;
pub mod page;
pub mod pmap;

/// Sets up virtual memory, on the boot CPU.
pub fn init() {
    page::setup();
    object::bootstrap();
}
