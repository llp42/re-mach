// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Physical pages.

use core::ops::Range;

use super::address::PhysAddr;
use crate::sync::BootCell;

/// Physical memory loaded from boot memory.
struct Loaded {
    /// The memory the page array will cover, holes included.
    span: Option<Range<PhysAddr>>,
    /// The boot heap's unused part, free from the start.
    heap: Option<Range<PhysAddr>>,
}

static LOADED: BootCell<Loaded> = BootCell::new(Loaded {
    span: None,
    heap: None,
});

/// Widens the span to cover `range`.
pub fn load(range: Range<PhysAddr>) {
    let mut loaded = LOADED.borrow_mut();
    loaded.span = Some(widen(loaded.span.take(), range));
}

/// Widens the free heap to cover `range`.
pub fn load_heap(range: Range<PhysAddr>) {
    let mut loaded = LOADED.borrow_mut();
    loaded.heap = Some(widen(loaded.heap.take(), range));
}

/// `range`, widened to cover `area` if there is one.
fn widen(area: Option<Range<PhysAddr>>, range: Range<PhysAddr>) -> Range<PhysAddr> {
    match area {
        Some(area) => area.start.min(range.start)..area.end.max(range.end),
        None => range,
    }
}
