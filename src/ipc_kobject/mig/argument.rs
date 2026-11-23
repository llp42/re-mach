// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::marker::PhantomData;

use crate::ipc::MessageType;
use crate::ipc::port::port_name::{PortName, PortType};
use crate::slab::CacheBox;
use crate::vm::VirtualAddress;
use crate::vm::map::copy::MapCopy;
use crate::vm::object::offset::Offset;

/// An argument as MIG lays it out: its type word, then its data, padded to 8 bytes.
#[repr(C)]
pub struct Argument<T> {
    /// Its type word.
    pub word: u64,
    /// Its data.
    pub data: T,
}

/// MIG's type word, Mach's `mach_msg_type_t`, for `number` inline items of
/// type `name`, `size` bits each.
pub const fn type_word(name: MessageType, size: u16, number: u32) -> u64 {
    // The inline bit, bit 29.
    word(name, size, 1 << 5, number)
}

/// MIG's type word for `number` items of type `name`, `size` bits each,
/// with `flags` from bit 24.
pub(in crate::ipc_kobject) const fn word(
    name: MessageType,
    size: u16,
    flags: u8,
    number: u32,
) -> u64 {
    let [size_low, size_high] = size.to_le_bytes();
    let [number_0, number_1, number_2, number_3] = number.to_le_bytes();
    // The name, the size, then the flags; the number above.
    u64::from_le_bytes([
        name.number(),
        size_low,
        size_high,
        flags,
        number_0,
        number_1,
        number_2,
        number_3,
    ])
}

/// Data MIG carries inline after its type word.
///
/// # Safety
///
/// `#[repr(C)]`, `#[repr(transparent)]` or primitive, aligned to at most 8
/// bytes, every byte in a field (no padding), and any bytes make one.
pub unsafe trait Inline: Copy {
    /// MIG's type word for it.
    const WORD: u64;
    /// The word's bits MIG's stub checks: all of them.
    const CHECKED: u64 = u64::MAX;
}

/// An element of MIG's arrays.
///
/// # Safety
///
/// As `Inline`.
pub(in crate::ipc_kobject) unsafe trait Element: Copy {
    /// Its type name.
    const NAME: MessageType;
    /// Its size, in bits.
    const SIZE: u16;

    /// MIG's type word for `number` of them inline.
    fn word(number: u32) -> u64 {
        type_word(Self::NAME, Self::SIZE, number)
    }

    /// MIG's type word for `number` of them out of line.
    fn out_of_line_word(number: u32) -> u64 {
        // The long-form bit, bit 30; not inline.
        word(Self::NAME, Self::SIZE, 1 << 6, number)
    }

    /// How many of an inline array of `number` its routine reads: all.
    fn readable(number: usize) -> usize {
        number
    }
}

/// An out-of-line array a routine gives or gets: the copy holding it, and
/// how many elements.
pub(in crate::ipc_kobject) struct OutOfLine<T> {
    /// The copy holding it.
    copy: CacheBox<MapCopy>,
    /// How many elements.
    count: usize,
    /// Their type.
    element: PhantomData<T>,
}

impl<T: Element> OutOfLine<T> {
    /// `count` elements in `copy`.
    pub(in crate::ipc_kobject) const fn new(copy: CacheBox<MapCopy>, count: usize) -> Self {
        Self {
            copy,
            count,
            element: PhantomData,
        }
    }

    /// Its type word, and the copy's address, which the reply takes.
    ///
    /// # Panics
    ///
    /// If the count is past 32 bits.
    pub(in crate::ipc_kobject) fn into_parts(self) -> (u64, usize) {
        let word = T::out_of_line_word(u32::try_from(self.count).unwrap());
        (
            word,
            CacheBox::into_raw(self.copy).as_ptr().expose_provenance(),
        )
    }

    /// The copy holding it.
    pub(in crate::ipc_kobject) fn into_copy(self) -> CacheBox<MapCopy> {
        self.copy
    }
}

// SAFETY: a primitive.
unsafe impl Inline for u16 {
    const WORD: u64 = type_word(MessageType::Integer16, 16, 1);
}

// SAFETY: a primitive.
unsafe impl Inline for i32 {
    const WORD: u64 = type_word(MessageType::Integer32, 32, 1);
}

// SAFETY: a primitive.
unsafe impl Inline for u32 {
    const WORD: u64 = type_word(MessageType::Integer32, 32, 1);
}

// SAFETY: a primitive.
unsafe impl Inline for usize {
    const WORD: u64 = type_word(MessageType::Integer64, 64, 1);
}

// SAFETY: `repr(transparent)` over a `usize`.
unsafe impl Inline for VirtualAddress {
    const WORD: u64 = type_word(MessageType::Integer64, 64, 1);
}

// SAFETY: `repr(transparent)` over a `usize`.
unsafe impl Inline for Offset {
    const WORD: u64 = type_word(MessageType::Integer64, 64, 1);
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Inline for PortName {
    const WORD: u64 = type_word(MessageType::PortName, 32, 1);
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Inline for PortType {
    const WORD: u64 = type_word(MessageType::Integer32, 32, 1);
}

// SAFETY: a primitive.
unsafe impl Element for u8 {
    const NAME: MessageType = MessageType::Byte;
    const SIZE: u16 = 8;
}

// SAFETY: a primitive.
unsafe impl Element for i32 {
    const NAME: MessageType = MessageType::Integer32;
    const SIZE: u16 = 32;
}

// SAFETY: a primitive.
unsafe impl Element for u32 {
    const NAME: MessageType = MessageType::Integer32;
    const SIZE: u16 = 32;
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Element for PortName {
    const NAME: MessageType = MessageType::PortName;
    const SIZE: u16 = 32;
}

// SAFETY: `repr(transparent)` over a `u32`.
unsafe impl Element for PortType {
    const NAME: MessageType = MessageType::Integer32;
    const SIZE: u16 = 32;
}
