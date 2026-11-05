// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Page arithmetic.

use core::ops::{Add, Sub};
use core::ptr;

use super::memory_layout::{DIRECT_MAP_BASE, KERNEL_BASE, PAGE_SIZE};

/// Physical address, for page arithmetic.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(pub usize);

impl PhysAddr {
    /// Rounds down to a page boundary.
    pub const fn align_down(self) -> Self {
        Self(self.0 & !(PAGE_SIZE - 1))
    }

    /// Rounds up to a page boundary; `None` on overflow.
    pub fn checked_align_up(self) -> Option<Self> {
        self.0.checked_next_multiple_of(PAGE_SIZE).map(Self)
    }

    /// `self` plus `length`; `None` on overflow.
    pub fn checked_add(self, length: usize) -> Option<Self> {
        self.0.checked_add(length).map(Self)
    }

    /// Bytes from `other` up to `self`; `None` if `other` is above `self`.
    pub const fn checked_sub(self, other: Self) -> Option<usize> {
        self.0.checked_sub(other.0)
    }

    /// Its address in the kernel window, which maps physical memory from
    /// `KERNEL_BASE`.
    pub const fn kernel_window_ptr<T>(self) -> *mut T {
        ptr::with_exposed_provenance_mut(self.0 + KERNEL_BASE)
    }

    /// Its address in the direct map, which maps all physical memory from
    /// `DIRECT_MAP_BASE`.
    pub const fn direct_map_ptr<T>(self) -> *mut T {
        ptr::with_exposed_provenance_mut(self.0 + DIRECT_MAP_BASE)
    }

    /// The address the direct map maps at `pointer`.
    pub fn from_direct_map<T>(pointer: *const T) -> Self {
        Self(pointer.addr() - DIRECT_MAP_BASE)
    }
}

impl Add<usize> for PhysAddr {
    type Output = Self;

    fn add(self, length: usize) -> Self {
        Self(self.0 + length)
    }
}

impl Sub for PhysAddr {
    type Output = usize;

    fn sub(self, other: Self) -> usize {
        self.0 - other.0
    }
}
