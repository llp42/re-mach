// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads.

pub mod create;
pub mod halt;
pub mod info;
pub mod machine;
pub mod ports;
pub mod priority;
pub mod reaper;
pub mod run;
pub mod state;
pub mod stop;
#[expect(clippy::module_inception, reason = "the file is named for its type")]
pub mod thread;
pub mod timeout;
