// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Machine-independent kernel code.

pub mod console;
pub mod host;
pub mod machine;
mod panic;
pub mod sched;
pub mod startup;
pub mod thread;
