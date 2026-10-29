// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The boot services table.

use core::ffi::c_void;

use super::{MemoryDescriptor, Status, TableHeader};

/// `EFI_BOOT_SERVICES`.
#[repr(C)]
pub struct BootServices {
    pub hdr: TableHeader,

    // Task priority services
    pub raise_tpl: *mut c_void,
    pub restore_tpl: *mut c_void,

    // Memory services
    pub allocate_pages: *mut c_void,
    pub free_pages: *mut c_void,
    pub get_memory_map: unsafe extern "efiapi" fn(
        memory_map_size: *mut usize,
        memory_map: *mut MemoryDescriptor,
        map_key: *mut usize,
        descriptor_size: *mut usize,
        descriptor_version: *mut u32,
    ) -> Status,
    pub allocate_pool: *mut c_void,
    pub free_pool: *mut c_void,

    // Event and timer services
    pub create_event: *mut c_void,
    pub set_timer: *mut c_void,
    pub wait_for_event: *mut c_void,
    pub signal_event: *mut c_void,
    pub close_event: *mut c_void,
    pub check_event: *mut c_void,

    // Protocol handler services
    pub install_protocol_interface: *mut c_void,
    pub reinstall_protocol_interface: *mut c_void,
    pub uninstall_protocol_interface: *mut c_void,
    pub handle_protocol: *mut c_void,
    pub reserved: *mut c_void,
    pub register_protocol_notify: *mut c_void,
    pub locate_handle: *mut c_void,
    pub locate_device_path: *mut c_void,
    pub install_configuration_table: *mut c_void,

    // Image services
    pub load_image: *mut c_void,
    pub start_image: *mut c_void,
    pub exit: *mut c_void,
    pub unload_image: *mut c_void,
    #[expect(clippy::struct_field_names, reason = "the spec's name")]
    pub exit_boot_services:
        unsafe extern "efiapi" fn(image_handle: *mut c_void, map_key: usize) -> Status,

    // Miscellaneous services
    pub get_next_monotonic_count: *mut c_void,
    pub stall: *mut c_void,
    pub set_watchdog_timer: *mut c_void,

    // Driver support services
    pub connect_controller: *mut c_void,
    pub disconnect_controller: *mut c_void,

    // Open and close protocol services
    pub open_protocol: *mut c_void,
    pub close_protocol: *mut c_void,
    pub open_protocol_information: *mut c_void,

    // Library services
    pub protocols_per_handle: *mut c_void,
    pub locate_handle_buffer: *mut c_void,
    pub locate_protocol: *mut c_void,
    pub install_multiple_protocol_interfaces: *mut c_void,
    pub uninstall_multiple_protocol_interfaces: *mut c_void,

    // 32-bit CRC services
    pub calculate_crc32: *mut c_void,

    // Miscellaneous services
    pub copy_mem: *mut c_void,
    pub set_mem: *mut c_void,
    pub create_event_ex: *mut c_void,
}
