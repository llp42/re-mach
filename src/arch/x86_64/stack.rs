// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Stacks the CPU runs on.

use core::cell::UnsafeCell;

use super::PAGE_SIZE;

/// A `SIZE`-byte stack above a guard page, used through RSP or an IST slot.
#[repr(C, align(4096))]
pub struct Stack<const SIZE: usize> {
    /// Left unmapped by the pmap, so an overflow faults.
    guard: UnsafeCell<[u8; PAGE_SIZE]>,
    bytes: UnsafeCell<[u8; SIZE]>,
}

// SAFETY: the bytes are only used as a stack, never through this value.
unsafe impl<const SIZE: usize> Sync for Stack<SIZE> {}

impl<const SIZE: usize> Stack<SIZE> {
    pub const fn new() -> Self {
        Self {
            guard: UnsafeCell::new([0; PAGE_SIZE]),
            bytes: UnsafeCell::new([0; SIZE]),
        }
    }

    /// The address past the end, where the CPU starts pushing.
    pub fn top(&self) -> u64 {
        u64::try_from(self.bytes.get().expose_provenance() + SIZE).unwrap()
    }

    /// The guard page's address.
    pub fn guard(&self) -> u64 {
        u64::try_from(self.guard.get().expose_provenance()).unwrap()
    }
}
