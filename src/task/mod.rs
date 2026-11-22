// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Tasks: an address space, a name space for ports, and threads.

pub mod control;
pub mod create;
pub mod halt;
pub mod info;
pub mod members;
pub mod ports;
#[expect(clippy::module_inception, reason = "the file is named for its type")]
pub mod task;
