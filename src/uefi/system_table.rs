// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The system table.

use core::ffi::c_void;

use super::{
    BootServices, MemoryDescriptor, MemoryMap, SimpleTextOutputProtocol, Status, TableHeader,
};

/// `EFI_SYSTEM_TABLE`.
#[repr(C)]
pub struct SystemTable {
    pub hdr: TableHeader,
    pub firmware_vendor: *mut u16,
    pub firmware_revision: u32,
    pub console_in_handle: *mut c_void,
    pub con_in: *mut c_void,
    pub console_out_handle: *mut c_void,
    pub con_out: *mut SimpleTextOutputProtocol,
    pub standard_error_handle: *mut c_void,
    pub std_err: *mut c_void,
    pub runtime_services: *mut c_void,
    pub boot_services: *mut BootServices,
    pub number_of_table_entries: usize,
    pub configuration_table: *mut c_void,
}

impl SystemTable {
    /// Reads the memory map into `buffer` and exits boot services.
    ///
    /// Errors: the firmware's status, or `UNSUPPORTED` for an unusable entry
    /// size; boot services may still be active. `system_table` is a raw
    /// pointer because the firmware writes to it here.
    ///
    /// # Safety
    ///
    /// Arguments from the entry point; boot services active.
    pub unsafe fn exit_boot_services<'a>(
        system_table: *mut Self,
        image: *mut c_void,
        buffer: &'a mut [u64],
    ) -> Result<MemoryMap<'a>, Status> {
        // SAFETY: guaranteed by the caller.
        let boot_services = unsafe { &*(*system_table).boot_services };
        loop {
            let mut map_size = size_of_val(buffer);
            let mut map_key = 0;
            let mut descriptor_size = 0;
            let mut descriptor_version = 0;
            // SAFETY: valid pointers; `map_size` is `buffer`'s size.
            let status = unsafe {
                (boot_services.get_memory_map)(
                    &raw mut map_size,
                    buffer.as_mut_ptr().cast(),
                    &raw mut map_key,
                    &raw mut descriptor_size,
                    &raw mut descriptor_version,
                )
            };
            if status != Status::SUCCESS {
                return Err(status);
            }
            if descriptor_size < size_of::<MemoryDescriptor>()
                || !descriptor_size.is_multiple_of(align_of::<MemoryDescriptor>())
            {
                return Err(Status::UNSUPPORTED);
            }
            // SAFETY: valid `image`; current `map_key`.
            match unsafe { (boot_services.exit_boot_services)(image, map_key) } {
                Status::SUCCESS => {
                    let buffer: &'a [u64] = buffer;
                    return Ok(MemoryMap::new(&buffer[..map_size / 8], descriptor_size));
                }
                // Stale map key: retry.
                Status::INVALID_PARAMETER => {}
                status => return Err(status),
            }
        }
    }
}
