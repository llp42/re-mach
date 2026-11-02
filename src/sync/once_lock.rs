// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! A value set once, then shared.

use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::sync::atomic::{AtomicBool, Ordering};

/// A value set once, then shared; a second `set` fails instead of waiting.
pub struct OnceLock<T> {
    /// Whether a `set` took the cell.
    taken: AtomicBool,
    /// Whether the value is written.
    ready: AtomicBool,
    value: UnsafeCell<MaybeUninit<T>>,
}

// SAFETY: the one `set` that takes the cell writes the value before
// releasing `ready`; readers share it only after acquiring `ready`.
unsafe impl<T: Send + Sync> Sync for OnceLock<T> {}

impl<T> OnceLock<T> {
    /// An empty cell.
    pub const fn new() -> Self {
        Self {
            taken: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Sets the value, or gives it back if the cell was already taken.
    pub fn set(&self, value: T) -> Result<(), T> {
        if self.taken.swap(true, Ordering::Relaxed) {
            return Err(value);
        }
        // SAFETY: only this `set` took the cell, and no reader looks before
        // `ready`.
        unsafe { (*self.value.get()).write(value) };
        self.ready.store(true, Ordering::Release);
        Ok(())
    }

    /// The value, once set.
    pub fn get(&self) -> Option<&T> {
        self.ready
            .load(Ordering::Acquire)
            // SAFETY: `ready` is set once the value is written, and the
            // value is never written again.
            .then(|| unsafe { (*self.value.get()).assume_init_ref() })
    }
}

impl<T> Default for OnceLock<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> Drop for OnceLock<T> {
    fn drop(&mut self) {
        if *self.ready.get_mut() {
            // SAFETY: written by `set`, and dropped only here.
            unsafe { self.value.get_mut().assume_init_drop() };
        }
    }
}
