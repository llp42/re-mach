// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Memory objects' kernel side.

pub mod external;
pub mod init;
#[expect(clippy::module_inception, reason = "the file is named for its type")]
pub mod object;
pub mod offset;
pub mod shadow;
pub mod state;
