// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Machine-independent kernel core, like dfly's `sys/kern`.

mod init;
mod thread;

pub use init::THREAD0;
pub use thread::Thread;
