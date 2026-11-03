// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! State only the boot CPU touches.

use core::cell::{Ref, RefCell, RefMut};

use crate::smp;

/// State only the boot CPU touches, before other CPUs start: a `RefCell` a
/// static can hold, so overlapping borrows panic, interrupts included.
pub struct BootCell<T>(RefCell<T>);

// SAFETY: only the boot CPU borrows it, which `borrow` and `borrow_mut`
// check, so no other CPU races the `RefCell`'s borrow count; the count
// stops overlapping borrows.
unsafe impl<T: Send> Sync for BootCell<T> {}

impl<T> BootCell<T> {
    pub const fn new(value: T) -> Self {
        Self(RefCell::new(value))
    }

    /// Borrows the state.
    ///
    /// # Panics
    ///
    /// Off the boot CPU, or if it is borrowed mutably.
    pub fn borrow(&self) -> Ref<'_, T> {
        on_boot_cpu();
        self.0.borrow()
    }

    /// Borrows the state mutably.
    ///
    /// # Panics
    ///
    /// Off the boot CPU, or if it is borrowed.
    pub fn borrow_mut(&self) -> RefMut<'_, T> {
        on_boot_cpu();
        self.0.borrow_mut()
    }
}

/// Checks that the current CPU is the boot CPU.
fn on_boot_cpu() {
    assert!(
        smp::current().id() == 0,
        "boot cell: touched off the boot CPU"
    );
}
