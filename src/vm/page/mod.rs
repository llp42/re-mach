// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Physical pages.

pub mod alloc;
pub mod load;
#[expect(clippy::module_inception, reason = "the file is named for its type")]
pub mod page;
pub mod pool;
pub mod queue;
pub mod reserve;
