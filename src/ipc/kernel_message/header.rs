// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Message headers: Mach's layout and its bits.

use crate::ipc::MessageType;

/// A message's header, in Mach's layout. Its ports are names as the user
/// sent them, then, once copied in, ports' exposed addresses.
#[repr(C)]
pub struct Header {
    /// Its ports' right types, and whether it is complex.
    pub bits: MessageBits,
    /// Its size, in bytes.
    pub size: u32,
    /// Its destination.
    pub remote_port: usize,
    /// Its reply port.
    pub local_port: usize,
    /// Its sequence number on the port it is received from.
    pub sequence_number: u32,
    /// What it asks for.
    pub id: i32,
}

/// A message header's bits, as Mach's: its ports' right types, and whether
/// it is complex.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct MessageBits(u32);

impl MessageBits {
    /// The bit of a message whose body holds rights or out-of-line memory,
    /// `MACH_MSGH_BITS_COMPLEX`.
    const COMPLEX: u32 = 1 << 31;

    /// The bits of a simple message with right types `remote` for its
    /// destination and `local` for its reply port, as `MACH_MSGH_BITS` makes
    /// them; `None`, type 0, for no port.
    pub const fn new(remote: Option<MessageType>, local: Option<MessageType>) -> Self {
        let remote = match remote {
            Some(message_type) => message_type.number(),
            None => 0,
        };
        let local = match local {
            Some(message_type) => message_type.number(),
            None => 0,
        };
        Self(u32::from_le_bytes([remote, local, 0, 0]))
    }

    /// Its destination's right type, as `MACH_MSGH_BITS_REMOTE` gives it, if
    /// it has one.
    ///
    /// # Panics
    ///
    /// If the type is not one used here: not yet.
    pub fn remote(self) -> Option<MessageType> {
        let [remote, ..] = self.0.to_le_bytes();
        (remote != 0).then(|| {
            MessageType::try_from(remote)
                .unwrap_or_else(|number| panic!("MACH_MSGH_BITS_REMOTE: type {number}, not yet"))
        })
    }

    /// Its reply port's right type, as `MACH_MSGH_BITS_LOCAL` gives it, if
    /// it has one.
    ///
    /// # Panics
    ///
    /// If the type is not one used here: not yet.
    pub fn local(self) -> Option<MessageType> {
        let [_, local, ..] = self.0.to_le_bytes();
        (local != 0).then(|| {
            MessageType::try_from(local)
                .unwrap_or_else(|number| panic!("MACH_MSGH_BITS_LOCAL: type {number}, not yet"))
        })
    }

    /// Whether its message's body holds rights or out-of-line memory.
    pub const fn is_complex(self) -> bool {
        self.0 & Self::COMPLEX != 0
    }

    /// The same bits, complex.
    pub const fn with_complex(self) -> Self {
        Self(self.0 | Self::COMPLEX)
    }
}

impl From<MessageBits> for u32 {
    fn from(bits: MessageBits) -> Self {
        bits.0
    }
}
