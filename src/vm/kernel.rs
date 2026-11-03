// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Kernel memory: the kernel's map.

use super::map::Map;
use super::pmap::MachinePmap as _;
use crate::arch::{self, MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE};
use crate::sync::OnceLock;

/// The kernel's map, once `init` sets it up.
pub static KERNEL_MAP: OnceLock<Map> = OnceLock::new();

/// Sets up the kernel's map, on the boot CPU.
///
/// # Panics
///
/// On a second call.
pub fn init() {
    // A range can't end past the last page, so the map stops before it.
    let bounds = MIN_KERNEL_ADDRESS..MAX_KERNEL_ADDRESS - PAGE_SIZE + 1;
    assert!(
        KERNEL_MAP
            .set(Map::new(arch::Pmap::kernel(), bounds))
            .is_ok(),
        "kmem_init: set up twice"
    );
}
