// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! A vector held in place.

use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::slice;

/// A vector of up to `N` items, held in place.
pub struct ArrayVec<T: Copy, const N: usize> {
    items: [MaybeUninit<T>; N],
    len: usize,
}

impl<T: Copy, const N: usize> ArrayVec<T, N> {
    pub const fn new() -> Self {
        Self {
            items: [const { MaybeUninit::uninit() }; N],
            len: 0,
        }
    }

    /// Whether it holds `N` items.
    pub const fn is_full(&self) -> bool {
        self.len == N
    }

    /// Appends `item`.
    ///
    /// # Panics
    ///
    /// If it is full.
    pub fn push(&mut self, item: T) {
        assert!(!self.is_full(), "ArrayVec: full");
        self.items[self.len].write(item);
        self.len += 1;
    }

    /// Inserts `item` at `index`, moving the items after it up.
    ///
    /// # Panics
    ///
    /// If it is full, or `index` is past its end.
    pub fn insert(&mut self, index: usize, item: T) {
        assert!(!self.is_full(), "ArrayVec: full");
        assert!(index <= self.len, "ArrayVec: index past the end");
        self.items.copy_within(index..self.len, index + 1);
        self.items[index].write(item);
        self.len += 1;
    }

    /// Removes every item.
    pub const fn clear(&mut self) {
        self.len = 0;
    }
}

impl<T: Copy, const N: usize> Default for ArrayVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: Copy, const N: usize> Deref for ArrayVec<T, N> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        // SAFETY: the first `len` items are written.
        unsafe { slice::from_raw_parts(self.items.as_ptr().cast(), self.len) }
    }
}

impl<T: Copy, const N: usize> DerefMut for ArrayVec<T, N> {
    fn deref_mut(&mut self) -> &mut [T] {
        // SAFETY: the first `len` items are written.
        unsafe { slice::from_raw_parts_mut(self.items.as_mut_ptr().cast(), self.len) }
    }
}
