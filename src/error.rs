// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Why kernel calls fail.

/// Why a kernel call failed, numbered as Mach's return codes.
#[derive(Debug)]
pub enum Error {
    /// The address is not valid for it.
    InvalidAddress = 1,
    /// The memory is there, but doesn't allow the access asked for.
    ProtectionFailure = 2,
    /// The range is in use, no free range is large enough, or the map's size
    /// limit would be passed.
    NoSpace = 3,
    /// An argument is not valid for it.
    InvalidArgument = 4,
    /// The call does not apply to the object as it stands.
    Failure = 5,
    /// A resource it needed could not be allocated, perhaps for now only.
    ResourceShortage = 6,
    /// The name denotes a right already.
    NameExists = 13,
}
