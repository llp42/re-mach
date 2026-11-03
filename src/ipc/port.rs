// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Ports.

use crate::kernel::slab::Cache;

/// A port; so far only its reference count and whether it is alive.
#[expect(dead_code, reason = "ipc_port_alloc and the port calls use them")]
pub struct Port {
    /// How many references it has.
    references: u32,
    /// Whether it is alive.
    active: bool,
}

/// The cache ports come from.
pub(super) static CACHE: Cache<Port> = Cache::new("ipc_port");
