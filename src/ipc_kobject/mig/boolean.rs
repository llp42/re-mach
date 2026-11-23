// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::ipc::MessageType;

use super::argument::{Inline, type_word};

/// A boolean, MIG's `boolean_t`: true unless 0.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(in crate::ipc_kobject) struct Boolean(u32);

impl From<bool> for Boolean {
    fn from(boolean: bool) -> Self {
        Self(u32::from(boolean))
    }
}

impl From<Boolean> for bool {
    fn from(boolean: Boolean) -> Self {
        boolean.0 != 0
    }
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Inline for Boolean {
    const WORD: u64 = type_word(MessageType::Boolean, 32, 1);
}
