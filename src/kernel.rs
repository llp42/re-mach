// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Initialization shared by boot environments.

use crate::boot::info::{BootInfo, MemoryKind};
use crate::diagnostics::{self, dbg_println};

/// The boot adapter retains the blob and its reserved backing memory.
pub fn run(info: BootInfo<'_>) -> ! {
    let _image = info.image();
    let _framebuffer = info.framebuffer();
    let mut available_bytes = 0_u64;
    for region in info.memory_regions() {
        if region.kind == MemoryKind::Available {
            available_bytes = available_bytes
                .checked_add(region.byte_len)
                .unwrap_or_else(|| diagnostics::fatal(format_args!("memory inventory overflow")));
        }
    }
    dbg_println!(
        "kernel handoff: image={:#x}..{:#x}, available bytes={}, framebuffer={}",
        _image.start,
        _image.end,
        available_bytes,
        _framebuffer.is_some()
    );

    diagnostics::print(format_args!("re-mach: boot complete\r\n"));
    crate::arch::halt()
}
