// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Cells.

use core::cell::UnsafeCell;

/// `UnsafeCell`, but `Sync`: callers synchronize. Mirrors `core::cell::SyncUnsafeCell`.
#[repr(transparent)]
pub struct SyncUnsafeCell<T: ?Sized>(UnsafeCell<T>);

// SAFETY: every access through `get` is synchronized by the caller.
unsafe impl<T: ?Sized + Sync> Sync for SyncUnsafeCell<T> {}

impl<T> SyncUnsafeCell<T> {
    pub const fn new(value: T) -> Self {
        Self(UnsafeCell::new(value))
    }
}

impl<T: ?Sized> SyncUnsafeCell<T> {
    pub const fn get(&self) -> *mut T {
        self.0.get()
    }
}
