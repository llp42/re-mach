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

/// What a message carries, data or a right, by Mach's type names; only the
/// ones used here.
#[derive(Clone, Copy)]
pub enum MessageType {
    /// `MACH_MSG_TYPE_INTEGER_32`: 32-bit integers.
    Integer32,
    /// `MACH_MSG_TYPE_INTEGER_64`: 64-bit integers.
    Integer64,
    /// `MACH_MSG_TYPE_STRING_C`: a NUL-terminated string.
    StringC,
    /// `MACH_MSG_TYPE_PORT_SEND`: a send right.
    PortSend,
    /// `MACH_MSG_TYPE_PORT_SEND_ONCE`: a send-once right.
    PortSendOnce,
    /// `MACH_MSG_TYPE_COPY_SEND`: a send right copied from the sender's.
    CopySend,
    /// `MACH_MSG_TYPE_MAKE_SEND_ONCE`: a send-once right made from the
    /// sender's receive right.
    MakeSendOnce,
}

impl MessageType {
    /// Its number, Mach's `mach_msg_type_name_t`.
    pub const fn number(self) -> u8 {
        match self {
            Self::Integer32 => 2,
            Self::Integer64 => 11,
            Self::StringC => 12,
            Self::PortSend => 17,
            Self::PortSendOnce => 18,
            Self::CopySend => 19,
            Self::MakeSendOnce => 21,
        }
    }
}

impl TryFrom<u8> for MessageType {
    type Error = u8;

    /// The type of Mach's number `number`, if it is one used here.
    fn try_from(number: u8) -> Result<Self, u8> {
        match number {
            2 => Ok(Self::Integer32),
            11 => Ok(Self::Integer64),
            12 => Ok(Self::StringC),
            17 => Ok(Self::PortSend),
            18 => Ok(Self::PortSendOnce),
            19 => Ok(Self::CopySend),
            21 => Ok(Self::MakeSendOnce),
            _ => Err(number),
        }
    }
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
