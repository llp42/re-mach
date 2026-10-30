// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Page arithmetic.

use core::ops::{Add, Sub};

use super::memory_layout::PAGE_SIZE;

/// Physical address, for page arithmetic.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysAddr(pub u64);

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
    pub fn checked_add(self, length: u64) -> Option<Self> {
        self.0.checked_add(length).map(Self)
    }

    /// Bytes from `other` up to `self`; `None` if `other` is above `self`.
    pub const fn checked_sub(self, other: Self) -> Option<u64> {
        self.0.checked_sub(other.0)
    }
}

impl Add<u64> for PhysAddr {
    type Output = Self;

    fn add(self, length: u64) -> Self {
        Self(self.0 + length)
    }
}

impl Sub for PhysAddr {
    type Output = u64;

    fn sub(self, other: Self) -> u64 {
        self.0 - other.0
    }
}
