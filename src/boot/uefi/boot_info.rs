// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Boot information from UEFI.

use core::{ptr, slice};

use super::{Guid, MemoryMap, SystemTable};
use crate::boot::{BootInfo, MemoryKind, MemoryRegion};

/// A view over the firmware's data; copies nothing.
pub struct Uefi<'a> {
    system_table: &'a SystemTable,
    memory_map: MemoryMap<'a>,
}

impl<'a> Uefi<'a> {
    pub const fn new(system_table: &'a SystemTable, memory_map: MemoryMap<'a>) -> Self {
        Self {
            system_table,
            memory_map,
        }
    }
}

impl BootInfo for Uefi<'_> {
    fn acpi_rsdp(&self) -> Option<u64> {
        if self.system_table.configuration_table.is_null() {
            return None;
        }
        // SAFETY: the firmware's table, kept after `ExitBootServices`.
        let tables = unsafe {
            slice::from_raw_parts(
                self.system_table.configuration_table,
                self.system_table.number_of_table_entries,
            )
        };
        let find = |guid| tables.iter().find(|table| table.vendor_guid == guid);
        let table = find(Guid::ACPI_20_TABLE).or_else(|| find(Guid::ACPI_TABLE))?;
        u64::try_from(table.vendor_table.addr()).ok()
    }

    fn efi_system_table(&self) -> Option<u64> {
        u64::try_from(ptr::from_ref(self.system_table).addr()).ok()
    }

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
