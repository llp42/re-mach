// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The text output protocol.

use core::ffi::c_void;

use super::Status;

/// `EFI_SIMPLE_TEXT_OUTPUT_PROTOCOL`.
#[repr(C)]
pub struct SimpleTextOutputProtocol {
    pub reset: *mut c_void,
    pub output_string:
        unsafe extern "efiapi" fn(this: *mut SimpleTextOutputProtocol, string: *mut u16) -> Status,
    pub test_string: *mut c_void,
    pub query_mode: *mut c_void,
    pub set_mode: *mut c_void,
    pub set_attribute: *mut c_void,
    pub clear_screen: *mut c_void,
    pub set_cursor_position: *mut c_void,
    pub enable_cursor: *mut c_void,
    pub mode: *mut c_void,
}

impl SimpleTextOutputProtocol {
    /// Prints `s` through `OutputString`, in UCS-2 chunks.
    ///
    /// # Safety
    ///
    /// `this` from the firmware; boot services active.
    pub unsafe fn output_str(this: *mut Self, s: &str) -> Status {
        let mut units = s.encode_utf16().peekable();
        while units.peek().is_some() {
            // The last slot stays 0: the terminator.
            let mut chunk = [0u16; 64];
            for (slot, unit) in chunk[..63].iter_mut().zip(&mut units) {
                *slot = unit;
            }
            // SAFETY: guaranteed by the caller; `chunk` is null-terminated.
            let status = unsafe { ((*this).output_string)(this, chunk.as_mut_ptr()) };
            if status != Status::SUCCESS {
                return status;
            }
        }
        Status::SUCCESS
    }
}
