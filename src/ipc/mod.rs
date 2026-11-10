// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Interprocess communication: ports, port sets and the spaces that name
//! them.

pub mod entry;
pub mod kernel_message;
pub mod mach_msg;
pub mod mach_port;
mod object;
pub mod port;
pub mod port_set;
mod right;
pub mod space;

use crate::sync::OnceLock;
use crate::vm::map::Map;
use crate::{ipc_host, vm};

/// A right as a message carries it, by Mach's type names; only the ones
/// used here.
#[derive(Clone, Copy)]
pub enum MessageType {
    /// `MACH_MSG_TYPE_PORT_SEND`: a send right.
    PortSend,
}

/// The kernel's map for IPC buffers, such as out-of-line data in its
/// replies.
static KERNEL_MAP: OnceLock<Map> = OnceLock::new();

/// The least size of `KERNEL_MAP`.
const MIN_KERNEL_MAP_SIZE: usize = 8 << 20;

/// Sets up IPC, on the boot CPU.
pub fn bootstrap() {
    space::CACHE.register();
    entry::CACHE.register();
    port::CACHE.register();
    port_set::CACHE.register();
}

/// Finishes setting up IPC, on the boot CPU: the kernel's IPC map gets an
/// eighth of memory, and at least `MIN_KERNEL_MAP_SIZE`; then the ports of
/// the host, the default set and the boot processor.
pub fn init() {
    vm::kernel::submap(
        &KERNEL_MAP,
        MIN_KERNEL_MAP_SIZE.max(vm::page::span_size() / 8),
    );
    ipc_host::init();
}
