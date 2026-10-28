// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Boot information from UEFI.

use super::MemoryMap;
use crate::boot::{BootInfo, MemoryKind, MemoryRegion};

/// A view over the firmware's data; copies nothing.
pub struct Uefi<'a> {
    memory_map: MemoryMap<'a>,
}

impl<'a> Uefi<'a> {
    pub const fn new(memory_map: MemoryMap<'a>) -> Self {
        Self { memory_map }
    }
}

impl BootInfo for Uefi<'_> {
    fn memory_map(&self) -> impl Iterator<Item = MemoryRegion> {
        self.memory_map.clone().map(|descriptor| MemoryRegion {
            address: descriptor.physical_start,
            length: descriptor.number_of_pages * 4096,
            kind: match descriptor.ty {
                // Loader, boot services, conventional: free after `ExitBootServices`.
                1..=4 | 7 => MemoryKind::Available,
                9 => MemoryKind::AcpiReclaimable,
                10 => MemoryKind::Nvs,
                // Unusable: errors detected.
                8 => MemoryKind::BadRam,
                // Reserved, runtime services, MMIO, unaccepted and the rest.
                _ => MemoryKind::Reserved,
            },
        })
    }
}
