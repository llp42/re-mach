// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Stacks the CPU runs on.

use core::cell::UnsafeCell;

use x86_64::VirtAddr;

/// A stack of `SIZE` bytes, page-aligned.
#[repr(align(4096))]
pub struct Stack<const SIZE: usize>(UnsafeCell<[u8; SIZE]>);

// SAFETY: only the CPU running on it uses it, through `%rsp`.
unsafe impl<const SIZE: usize> Sync for Stack<SIZE> {}

impl<const SIZE: usize> Stack<SIZE> {
    pub const fn new() -> Self {
        Self(UnsafeCell::new([0; SIZE]))
    }

    /// Past its last byte, where the CPU starts pushing.
    pub fn top(&self) -> VirtAddr {
        VirtAddr::from_ptr(self.0.get().cast::<u8>().wrapping_add(SIZE))
    }
}
