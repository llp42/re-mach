// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! CPU 0's boot stack.

use core::cell::UnsafeCell;

const SIZE: usize = 16 * 1024;

#[repr(C, align(4096))]
struct Stack(UnsafeCell<[u8; SIZE]>);

// SAFETY: only CPU 0 uses it, through raw pointers.
unsafe impl Sync for Stack {}

static STACK: Stack = Stack(UnsafeCell::new([0; SIZE]));

/// Initial stack pointer: the end of the stack, less 16 bytes.
pub fn top() -> *mut u8 {
    STACK.0.get().cast::<u8>().wrapping_add(SIZE - 16)
}
