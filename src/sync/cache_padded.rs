// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Values alone on their cache lines.

use core::ops::{Deref, DerefMut};

/// A value alone on its cache lines, so CPUs writing neighbours don't share
/// one.
#[repr(align(64))]
pub struct CachePadded<T>(T);

impl<T> CachePadded<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }
}

impl<T> Deref for CachePadded<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for CachePadded<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
