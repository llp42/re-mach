// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Why kernel calls fail.

/// Why a kernel call failed, numbered as Mach's return codes.
#[derive(Debug)]
pub enum Error {
    /// A resource it needed could not be allocated, perhaps for now only.
    ResourceShortage = 6,
}
