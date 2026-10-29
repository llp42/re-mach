// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! GUIDs.

/// `EFI_GUID`.
#[repr(C)]
#[derive(PartialEq, Eq)]
pub struct Guid {
    pub data1: u32,
    pub data2: u16,
    pub data3: u16,
    pub data4: [u8; 8],
}

impl Guid {
    /// `EFI_ACPI_20_TABLE_GUID`.
    pub const ACPI_20_TABLE: Self = Self {
        data1: 0x8868_e871,
        data2: 0xe4f1,
        data3: 0x11d3,
        data4: [0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81],
    };

    /// `ACPI_TABLE_GUID`.
    pub const ACPI_TABLE: Self = Self {
        data1: 0xeb9d_2d30,
        data2: 0x2d88,
        data3: 0x11d3,
        data4: [0x9a, 0x16, 0x00, 0x90, 0x27, 0x3f, 0xc1, 0x4d],
    };
}
