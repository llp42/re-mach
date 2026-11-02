// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! State only the boot CPU touches.

use core::cell::{Ref, RefCell, RefMut};

/// State only the boot CPU touches, before other CPUs start: a `RefCell` a
/// static can hold, so overlapping borrows panic, interrupts included.
pub struct BootCell<T>(RefCell<T>);

// SAFETY: until other CPUs start, only the boot CPU runs, so nothing
// races the `RefCell`'s borrow count; the count stops overlapping borrows.
unsafe impl<T: Send> Sync for BootCell<T> {}

impl<T> BootCell<T> {
    pub const fn new(value: T) -> Self {
        Self(RefCell::new(value))
    }

    /// Borrows the state.
    ///
    /// # Panics
    ///
    /// If it is borrowed mutably.
    pub fn borrow(&self) -> Ref<'_, T> {
        self.0.borrow()
    }

    /// Borrows the state mutably.
    ///
    /// # Panics
    ///
    /// If it is borrowed.
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        self.0.borrow_mut()
    }
}
