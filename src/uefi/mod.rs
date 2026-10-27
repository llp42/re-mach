// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Minimal UEFI bindings.
//!
//! Spec names in Rust case. Handles and unused entries are `*mut c_void`.

mod boot_services;
mod memory_descriptor;
mod memory_map;
mod simple_text_output_protocol;
mod status;
mod system_table;
mod table_header;

pub use boot_services::BootServices;
pub use memory_descriptor::MemoryDescriptor;
pub use memory_map::MemoryMap;
pub use simple_text_output_protocol::SimpleTextOutputProtocol;
pub use status::Status;
pub use system_table::SystemTable;
pub use table_header::TableHeader;

/// `EfiBootServicesCode`: free once nothing uses the firmware's code.
pub const BOOT_SERVICES_CODE: u32 = 3;

/// `EfiBootServicesData`: free once nothing uses the firmware's data.
pub const BOOT_SERVICES_DATA: u32 = 4;

/// `EfiConventionalMemory`: free once boot services exit.
pub const CONVENTIONAL_MEMORY: u32 = 7;

/// `EFI_PAGE_SIZE`: the unit of `MemoryDescriptor::number_of_pages`.
pub const PAGE_SIZE: u64 = 4096;

/// `EFI_PHYSICAL_ADDRESS`.
pub type PhysicalAddress = u64;

/// `EFI_VIRTUAL_ADDRESS`.
pub type VirtualAddress = u64;
