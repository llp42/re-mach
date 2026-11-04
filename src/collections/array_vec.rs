// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! A vector held in place.

use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::{ptr, slice};

/// A vector of up to `N` items, held in place.
pub struct ArrayVec<T, const N: usize> {
    items: [MaybeUninit<T>; N],
    len: usize,
}

impl<T, const N: usize> ArrayVec<T, N> {
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
        let at = self.items[index..].as_mut_ptr();
        // SAFETY: the items from `index` are written, and one more place
        // follows them since it is not full.
        unsafe { ptr::copy(at, at.add(1), self.len - index) };
        self.items[index].write(item);
        self.len += 1;
    }

    /// Keeps only the items `keep` holds to, in their order.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        let len = self.len;
        // Items not yet moved are out of reach if `keep` panics.
        self.len = 0;
        for index in 0..len {
            // SAFETY: the items from `index` on are still written, and this
            // one is read once.
            let item = unsafe { self.items[index].assume_init_read() };
            if keep(&item) {
                // At or below `index`, so not one still to be read.
                self.items[self.len].write(item);
                self.len += 1;
            }
        }
    }

    /// Removes every item.
    pub fn clear(&mut self) {
        let items: *mut [T] = &raw mut **self;
        self.len = 0;
        // SAFETY: the items were written, and are no longer reachable.
        unsafe { ptr::drop_in_place(items) };
    }
}

impl<T, const N: usize> Default for ArrayVec<T, N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T, const N: usize> Drop for ArrayVec<T, N> {
    fn drop(&mut self) {
        self.clear();
    }
}

impl<T, const N: usize> Deref for ArrayVec<T, N> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        // SAFETY: the first `len` items are written.
        unsafe { slice::from_raw_parts(self.items.as_ptr().cast(), self.len) }
    }
}

impl<T, const N: usize> DerefMut for ArrayVec<T, N> {
    fn deref_mut(&mut self) -> &mut [T] {
        // SAFETY: the first `len` items are written.
        unsafe { slice::from_raw_parts_mut(self.items.as_mut_ptr().cast(), self.len) }
    }
}
