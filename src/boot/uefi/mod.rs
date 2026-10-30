// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Minimal UEFI bindings.
//!
//! Spec names in Rust case. Handles and unused entries are `*mut c_void`.

mod boot_info;
mod boot_services;
mod configuration_table;
mod guid;
mod image;
mod memory_descriptor;
mod memory_map;
mod simple_text_output_protocol;
mod status;
mod system_table;
mod table_header;

pub use boot_info::Uefi;
pub use boot_services::{AllocateType, BootServices, MemoryType};
pub use configuration_table::ConfigurationTable;
pub use guid::Guid;
pub use image::move_image;
pub use memory_descriptor::MemoryDescriptor;
pub use memory_map::MemoryMap;
pub use simple_text_output_protocol::SimpleTextOutputProtocol;
pub use status::Status;
pub use system_table::SystemTable;
pub use table_header::TableHeader;

/// `EFI_PHYSICAL_ADDRESS`.
pub type PhysicalAddress = u64;

/// `EFI_VIRTUAL_ADDRESS`.
pub type VirtualAddress = u64;
