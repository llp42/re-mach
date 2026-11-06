// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! A task's machine part: the I/O ports it may use.

use crate::kalloc::SLAB_SIZE;
use crate::slab::{MIN_ALIGN, RawCache};

/// An I/O permission bitmap's size: a bit per port.
const IO_BITMAP_SIZE: usize = 0x1_0000 / 8;

/// The cache I/O permission bitmaps come from; larger than a page, so on
/// kalloc's slabs.
static IO_BITMAP_CACHE: RawCache =
    RawCache::new("i386_task_iopb", IO_BITMAP_SIZE, MIN_ALIGN, SLAB_SIZE);

/// Sets up tasks' machine part, on the boot CPU.
pub fn module_init() {
    IO_BITMAP_CACHE.register();
}
