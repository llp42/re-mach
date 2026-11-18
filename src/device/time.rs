// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The `time` device, which maps the mapped time.

use core::ptr;

use crate::arch::{PhysAddr, PhysMap};
use crate::clock::mapped_time::MAPPED_TIME;
use crate::vm::Protection;
use crate::vm::object::Offset;
use crate::vm::phys_map::MachinePhysMap as _;

/// The page of the mapped time, which every offset maps, as `timemmap`
/// gives it; `None` if `protection` allows writing.
///
/// # Panics
///
/// If the mapped time has no page yet.
pub(super) fn map(_offset: Offset, protection: Protection) -> Option<PhysAddr> {
    if protection.contains(Protection::WRITE) {
        return None;
    }
    let mapped = MAPPED_TIME.get().expect("timemmap: no mapped time");
    PhysMap::extract_kernel(ptr::from_ref(*mapped).addr())
}
