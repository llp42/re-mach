// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Physical maps.

pub mod flags;
pub mod flush;
pub mod machine;
#[expect(clippy::module_inception, reason = "the file is named for its type")]
pub mod phys_map;
pub mod table;
