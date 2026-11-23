// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::ipc::entry::Rights;
/// A port's name in a space, as Mach's `mach_port_name_t`.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PortName(u32);

impl PortName {
    /// `MACH_PORT_NAME_NULL`: no port.
    pub(in crate::ipc) const NULL: Self = Self(0);
    /// `MACH_PORT_NAME_DEAD`: a dead port.
    const DEAD: Self = Self(u32::MAX);

    /// Whether it names a port, as `MACH_PORT_NAME_VALID`: neither null nor
    /// dead.
    pub(in crate::ipc) fn is_valid(self) -> bool {
        self != Self::NULL && self != Self::DEAD
    }

    /// Its key in a space's map.
    pub(in crate::ipc) fn key(self) -> u64 {
        u64::from(self.0)
    }

    /// The name of `key` in a space's map.
    ///
    /// # Panics
    ///
    /// If `key` is past 32 bits.
    pub(in crate::ipc) fn from_key(key: u64) -> Self {
        Self(u32::try_from(key).unwrap())
    }
}

impl From<u32> for PortName {
    fn from(name: u32) -> Self {
        Self(name)
    }
}

impl From<PortName> for u32 {
    fn from(name: PortName) -> Self {
        name.0
    }
}

bitflags::bitflags! {
    /// The rights a name holds, Mach's `mach_port_type_t`; its bits come
    /// from `Rights`.
    #[repr(transparent)]
    #[derive(Clone, Copy)]
    pub struct PortType: u32 {}
}

impl From<Rights> for PortType {
    /// The type bits of `rights`, as `IE_BITS_TYPE` takes them.
    fn from(rights: Rights) -> Self {
        Self::from_bits_retain(u32::from(rights.bits()) << 16)
    }
}

/// A kind of right, Mach's `mach_port_right_t`; only the kinds used here.
pub(in crate::ipc) enum Right {
    /// `MACH_PORT_RIGHT_SEND`.
    Send,
    /// `MACH_PORT_RIGHT_RECEIVE`.
    Receive,
    /// `MACH_PORT_RIGHT_SEND_ONCE`.
    SendOnce,
    /// `MACH_PORT_RIGHT_PORT_SET`.
    PortSet,
    /// `MACH_PORT_RIGHT_DEAD_NAME`.
    DeadName,
}

impl TryFrom<u32> for Right {
    type Error = u32;

    /// The kind of Mach's number `number`, if it is one used here.
    fn try_from(number: u32) -> Result<Self, u32> {
        match number {
            0 => Ok(Self::Send),
            1 => Ok(Self::Receive),
            2 => Ok(Self::SendOnce),
            3 => Ok(Self::PortSet),
            4 => Ok(Self::DeadName),
            _ => Err(number),
        }
    }
}
