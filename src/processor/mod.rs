// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Processors and processor sets.

pub mod all_sets;
pub mod init;
pub mod members;
#[expect(clippy::module_inception, reason = "the file is named for its type")]
pub mod processor;
pub mod set;
pub mod state;
