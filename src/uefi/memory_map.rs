// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The final memory map.

use core::slice::ChunksExact;

use super::MemoryDescriptor;

/// Iterator over the final memory map.
pub struct MemoryMap<'a> {
    entries: ChunksExact<'a, u64>,
}

impl<'a> MemoryMap<'a> {
    /// `descriptor_size`: entry size in bytes, a non-zero multiple of 8.
    pub(super) fn new(map: &'a [u64], descriptor_size: usize) -> Self {
        Self {
            entries: map.chunks_exact(descriptor_size / 8),
        }
    }
}

impl Iterator for MemoryMap<'_> {
    type Item = MemoryDescriptor;

    fn next(&mut self) -> Option<MemoryDescriptor> {
        let entry = self.entries.next()?;
        if size_of_val(entry) < size_of::<MemoryDescriptor>() {
            return None;
        }
        // SAFETY: `entry` is large enough and `u64`-aligned; any bits are valid.
        Some(unsafe { entry.as_ptr().cast::<MemoryDescriptor>().read() })
    }
}
