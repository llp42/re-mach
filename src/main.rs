// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! re-mach: a minimal freestanding hello-world kernel for `x86_64`.

#![no_std]
#![no_main]

mod arch;
mod pe;
mod smp;
mod uefi;

use core::cell::UnsafeCell;
use core::ffi::c_void;
use core::fmt::{self, Write};
use core::panic::PanicInfo;

/// UEFI console; only valid while boot services are active.
struct Console(*mut uefi::SimpleTextOutputProtocol);

impl Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // SAFETY: built from the firmware's `con_out`; used before leaving boot services.
        let status = unsafe { uefi::SimpleTextOutputProtocol::output_str(self.0, s) };
        if status == uefi::Status::SUCCESS {
            Ok(())
        } else {
            Err(fmt::Error)
        }
    }
}

/// Memory map storage: static, so it outlives the firmware's stack.
struct MemoryMapBuffer(UnsafeCell<[u64; 2048]>);

// SAFETY: only `efi_main` uses it, once, on the boot CPU.
unsafe impl Sync for MemoryMapBuffer {}

static MEMORY_MAP_BUFFER: MemoryMapBuffer = MemoryMapBuffer(UnsafeCell::new([0; 2048]));

// UEFI entry point.
#[unsafe(no_mangle)]
extern "efiapi" fn efi_main(
    image: *mut c_void,
    system_table: *mut uefi::SystemTable,
) -> uefi::Status {
    // SAFETY: `system_table` comes from the firmware.
    let mut console = Console(unsafe { (*system_table).con_out });
    let _ = console.write_str("Hello, world! (re-mach)\r\n");

    // SAFETY: `efi_main` runs once, on one CPU; nothing else uses the buffer.
    let buffer = unsafe { &mut *MEMORY_MAP_BUFFER.0.get() };
    // SAFETY: arguments come from the firmware; boot services are active.
    let memory_map =
        match unsafe { uefi::SystemTable::exit_boot_services(system_table, image, buffer) } {
            Ok(memory_map) => memory_map,
            Err(status) => {
                let _ = write!(console, "ExitBootServices failed: {status:?}\r\n");
                return status;
            }
        };
    arch::init(memory_map)
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    arch::halt()
}
