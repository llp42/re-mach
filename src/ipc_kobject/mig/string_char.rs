// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::ipc::MessageType;

use super::argument::{Element, Inline, type_word};

/// A C string of `N` bytes, NUL-padded, MIG's `c_string[N]`.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(in crate::ipc_kobject) struct StringC<const N: usize>(pub(in crate::ipc_kobject) [u8; N]);

// SAFETY: bytes only.
unsafe impl<const N: usize> Inline for StringC<N> {
    const WORD: u64 = {
        // `N` in 32 bits, as `u32::try_from` would give it.
        assert!(N <= 0xffff_ffff, "StringC: too long");
        let [number_0, number_1, number_2, number_3, ..] = N.to_le_bytes();
        type_word(
            MessageType::StringC,
            8,
            u32::from_le_bytes([number_0, number_1, number_2, number_3]),
        )
    };
}

/// A character of a C string of variable length, MIG's `c_string[*: N]`.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(in crate::ipc_kobject) struct StringChar(u8);

impl From<StringChar> for u8 {
    fn from(character: StringChar) -> Self {
        character.0
    }
}

// SAFETY: `repr(transparent)` over a `u8`.
unsafe impl Element for StringChar {
    const NAME: MessageType = MessageType::StringC;
    const SIZE: u16 = 8;

    /// To the array's padding: its routine reads a C string to its NUL, as
    /// GNU Mach's do, whatever the number says.
    fn readable(number: usize) -> usize {
        number.next_multiple_of(8)
    }
}
