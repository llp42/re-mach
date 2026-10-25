// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Output written to the console device while no terminal backs it.
//!
//! The console device forwards writes to the terminal of the console
//! driver. When that driver has no terminal, writes come here and are
//! printed through the kernel's own console output, which reaches the same
//! hardware and the kernel message buffer.

use core::ffi::{c_int, c_void};

use crate::ffi;

/// Prints everything the write request `uio` holds; returns 0 or an errno.
/// Called by the console device's write routine.
#[unsafe(no_mangle)]
pub extern "C" fn rust_console_write(uio: *mut c_void) -> c_int {
    let mut buffer = [0_u8; 128];
    loop {
        // SAFETY: `uio` is the console write's request, valid for this
        // call; the glue moves at most `buffer.len()` bytes into `buffer`.
        let moved = unsafe { ffi::glue_uio_read(uio, buffer.as_mut_ptr().cast(), buffer.len()) };
        if moved <= 0 {
            return -moved;
        }
        // SAFETY: "%.*s" with a byte count no larger than the buffer.
        unsafe { ffi::kprintf(c"%.*s".as_ptr(), moved, buffer.as_ptr()) };
    }
}
