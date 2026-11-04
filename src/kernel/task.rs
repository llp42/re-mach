// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Tasks: an address space, a name space for ports, and threads.

use core::sync::atomic::AtomicU32;

use super::slab::Cache;
use crate::arch::{self, Machine as _};

/// A task; so far only its reference count and whether it is alive.
#[expect(dead_code, reason = "task_create_kernel and the task calls use them")]
pub struct Task {
    /// How many references it has.
    references: AtomicU32,
    /// Whether it is alive.
    active: bool,
}

/// The cache tasks come from.
static CACHE: Cache<Task> = Cache::new("task");

/// Sets up tasks, on the boot CPU.
pub fn init() {
    CACHE.register();
    arch::Current::task_module_init();
}
