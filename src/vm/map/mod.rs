// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Address maps.

pub mod change;
pub mod contents;
pub mod copy;
pub mod delete;
pub mod enter;
pub mod entry;
pub mod fork;
pub mod inherit;

#[expect(clippy::module_inception, reason = "the file is named for its type")]
pub mod map;

pub mod lookup;
pub mod protect;
pub mod region;
