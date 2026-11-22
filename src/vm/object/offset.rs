// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ops::Add;

/// A place in an object, in bytes from its start.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Offset(pub(super) usize);

impl From<usize> for Offset {
    fn from(offset: usize) -> Self {
        Self(offset)
    }
}

impl From<Offset> for usize {
    fn from(offset: Offset) -> Self {
        offset.0
    }
}

impl Add<usize> for Offset {
    type Output = Self;

    fn add(self, size: usize) -> Self {
        Self(self.0 + size)
    }
}
