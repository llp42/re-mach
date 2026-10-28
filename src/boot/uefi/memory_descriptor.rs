// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Memory map entries.

use super::{PhysicalAddress, VirtualAddress};

/// `EFI_MEMORY_DESCRIPTOR`.
#[repr(C)]
pub struct MemoryDescriptor {
    /// Spec `Type` (a Rust keyword). 7 is free memory.
    pub ty: u32,
    pub physical_start: PhysicalAddress,
    pub virtual_start: VirtualAddress,
    /// In 4KiB pages.
    pub number_of_pages: u64,
    pub attribute: u64,
}
