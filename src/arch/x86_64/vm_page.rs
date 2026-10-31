// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Physical pages.

use core::cell::UnsafeCell;
use core::ops::Range;

use super::address::PhysAddr;

/// Physical memory loaded from boot memory.
struct Loaded {
    /// The memory the page array will cover, holes included.
    span: UnsafeCell<Range<PhysAddr>>,
    /// The boot heap's unused part, free from the start.
    heap: UnsafeCell<Range<PhysAddr>>,
}

// SAFETY: only the boot CPU touches it.
unsafe impl Sync for Loaded {}

static LOADED: Loaded = Loaded {
    span: UnsafeCell::new(PhysAddr(0)..PhysAddr(0)),
    heap: UnsafeCell::new(PhysAddr(0)..PhysAddr(0)),
};

/// Widens the span to cover `range`.
///
/// # Safety
///
/// Runs on the boot CPU only.
pub unsafe fn load(range: Range<PhysAddr>) {
    // SAFETY: guaranteed by the caller.
    let span = unsafe { &mut *LOADED.span.get() };
    *span = if span.is_empty() {
        range
    } else {
        span.start.min(range.start)..span.end.max(range.end)
    };
}

/// Widens the free heap to cover `range`.
///
/// # Safety
///
/// Runs on the boot CPU only.
pub unsafe fn load_heap(range: Range<PhysAddr>) {
    // SAFETY: guaranteed by the caller.
    let heap = unsafe { &mut *LOADED.heap.get() };
    *heap = if heap.is_empty() {
        range
    } else {
        heap.start.min(range.start)..heap.end.max(range.end)
    };
}
