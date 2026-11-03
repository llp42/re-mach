// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Interprocess communication: ports, port sets and the spaces that name
//! them.

pub mod entry;
pub mod port;
pub mod port_set;
pub mod space;

/// Sets up IPC, on the boot CPU.
pub fn bootstrap() {
    space::CACHE.register();
    entry::CACHE.register();
    port::CACHE.register();
    port_set::CACHE.register();
}
