// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Why kernel calls fail.

/// Why a kernel call failed; `u32::from` gives Mach's return code for it.
#[derive(Debug)]
pub enum Error {
    /// The address is not valid for it.
    InvalidAddress,
    /// The memory is there, but doesn't allow the access asked for.
    ProtectionFailure,
    /// The range is in use, no free range is large enough, or the map's size
    /// limit would be passed.
    NoSpace,
    /// An argument is not valid for it.
    InvalidArgument,
    /// The call does not apply to the object as it stands.
    Failure,
    /// A resource it needed could not be allocated, perhaps for now only.
    ResourceShortage,
    /// The access restriction asked for is not allowed.
    NoAccess,
    /// The name denotes a right already.
    NameExists,
    /// The target is not an active task.
    InvalidTask,
    /// The target is not a host, or not the host port the call needs.
    InvalidHost,
    /// The name denotes no right.
    InvalidName,
    /// The name does not denote the right the call names.
    InvalidRight,
    /// An argument's value is invalid for the call.
    InvalidValue,
    /// The user references would overflow.
    UrefsOverflow,
    /// The wait's time ran out.
    TimedOut,
}

impl From<Error> for u32 {
    fn from(error: Error) -> Self {
        match error {
            Error::InvalidAddress => 1,
            Error::ProtectionFailure => 2,
            Error::NoSpace => 3,
            Error::InvalidArgument => 4,
            Error::Failure => 5,
            Error::ResourceShortage => 6,
            Error::NoAccess => 8,
            Error::NameExists => 13,
            Error::InvalidTask => 16,
            Error::InvalidHost => 22,
            Error::InvalidName => 15,
            Error::InvalidRight => 17,
            Error::InvalidValue => 18,
            Error::UrefsOverflow => 19,
            Error::TimedOut => 27,
        }
    }
}
