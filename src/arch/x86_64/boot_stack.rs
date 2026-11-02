// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! CPU 0's boot stack.

use super::stack::Stack;

/// Its size; `boot_header` starts 16 bytes below its end.
pub const SIZE: usize = 16 * 1024;

pub static STACK: Stack<SIZE> = Stack::new();
