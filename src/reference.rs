// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Counted references: handles that each hold one reference to an object.

use core::mem::ManuallyDrop;
use core::ops::Deref;
use core::ptr::NonNull;
use core::sync::atomic::{self, AtomicU32, Ordering};

/// A reference count, which only `Reference` changes.
pub struct Count(AtomicU32);

impl Count {
    /// A count of `references`.
    pub const fn new(references: u32) -> Self {
        Self(AtomicU32::new(references))
    }
}

/// An object that counts its references, freed when the last one drops.
///
/// # Safety
///
/// `references` gives the object's own count, and `free` may free the object
/// once that count reaches zero.
pub unsafe trait Counted {
    /// Its reference count.
    fn references(&self) -> &Count;

    /// Frees `object`, whose last reference dropped.
    ///
    /// # Safety
    ///
    /// Nothing uses `object` after.
    unsafe fn free(object: NonNull<Self>);
}

/// One reference to a `T`: a clone takes another, a drop drops it.
pub struct Reference<T: Counted>(NonNull<T>);

impl<T: Counted> Reference<T> {
    /// The handle of a reference to `object` its holder gives up.
    ///
    /// # Safety
    ///
    /// The caller holds a reference to `object`, which the handle takes.
    pub const unsafe fn from_raw(object: NonNull<T>) -> Self {
        Self(object)
    }

    /// Gives up the handle, keeping its reference, which `from_raw` takes
    /// back.
    pub fn into_raw(this: Self) -> NonNull<T> {
        ManuallyDrop::new(this).0
    }

    /// Its object's pointer; the handle keeps its reference.
    pub const fn as_ptr(this: &Self) -> NonNull<T> {
        this.0
    }
}

impl<T: Counted> Clone for Reference<T> {
    fn clone(&self) -> Self {
        T::references(self).0.fetch_add(1, Ordering::Relaxed);
        Self(self.0)
    }
}

impl<T: Counted> Drop for Reference<T> {
    fn drop(&mut self) {
        if T::references(self).0.fetch_sub(1, Ordering::Release) == 1 {
            // Every other holder's use comes before the free.
            atomic::fence(Ordering::Acquire);
            // SAFETY: that was its last reference.
            unsafe { T::free(self.0) };
        }
    }
}

impl<T: Counted> Deref for Reference<T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the reference keeps the object alive.
        unsafe { self.0.as_ref() }
    }
}

// SAFETY: a handle shares its object as `&T` does, and may drop it anywhere.
unsafe impl<T: Counted + Send + Sync> Send for Reference<T> {}

// SAFETY: as for `Send`.
unsafe impl<T: Counted + Send + Sync> Sync for Reference<T> {}
