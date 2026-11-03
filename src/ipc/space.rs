// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Spaces: a task's names for ports and port sets.

use crate::kernel::slab::Cache;

/// A space; so far only its reference count and whether it is in use.
#[expect(dead_code, reason = "ipc_space_create and the space calls use them")]
pub struct Space {
    /// How many references it has.
    references: u32,
    /// Whether it is in use; special spaces never are.
    active: bool,
}

impl Space {
    /// A special space: one reference, never in use.
    const fn special() -> Self {
        Self {
            references: 1,
            active: false,
        }
    }
}

/// The cache spaces come from.
pub(super) static CACHE: Cache<Space> = Cache::new("ipc_space");

/// The space of the ports the kernel receives; never in use.
#[expect(dead_code, reason = "ipc_port_alloc_kernel receives in it")]
pub static KERNEL_SPACE: Space = Space::special();

/// The space that marks ports receiving kernel replies; never in use.
#[expect(dead_code, reason = "kernel replies mark their ports with it")]
pub static REPLY_SPACE: Space = Space::special();
