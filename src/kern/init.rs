// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Boot-time setup, like dfly's `init_main.c`.

use core::ptr;
use core::sync::atomic::AtomicPtr;

use super::Thread;

/// The boot thread (dfly's `thread0`).
pub static THREAD0: Thread = Thread {
    per_cpu: AtomicPtr::new(ptr::null_mut()),
};
