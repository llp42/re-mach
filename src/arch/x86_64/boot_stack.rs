// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! CPU 0's boot stack.

use core::cell::UnsafeCell;

/// Its size; `boot_header` starts 16 bytes below its end.
pub const SIZE: usize = 16 * 1024;

#[repr(C, align(4096))]
pub struct Stack(UnsafeCell<[u8; SIZE]>);

// SAFETY: only CPU 0 uses it, through `%rsp`.
unsafe impl Sync for Stack {}

pub static STACK: Stack = Stack(UnsafeCell::new([0; SIZE]));
