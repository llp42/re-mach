// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Threads, like dfly's `struct thread` (`sys/thread.h`).

use core::sync::atomic::AtomicPtr;

use crate::smp::PerCpu;

/// A thread (dfly's `struct thread`).
pub struct Thread {
    /// The CPU it belongs to (`td_gd`).
    pub per_cpu: AtomicPtr<PerCpu>,
}
