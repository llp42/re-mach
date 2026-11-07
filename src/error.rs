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
    /// The name denotes a right already.
    NameExists,
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
            Error::NameExists => 13,
        }
    }
}
