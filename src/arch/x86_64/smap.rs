// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Supervisor mode access prevention: the kernel reaches user pages only
//! between an open site and a close site.

use core::{ptr, slice};

use x86_64::registers::control::{Cr4, Cr4Flags};

use super::address::PhysAddr;
use super::cpuid;
use super::memory_layout::KERNEL_BASE;

/// `stac`, an open site's instruction.
const OPEN: [u8; 3] = [0x0f, 0x01, 0xcb];

/// `clac`, a close site's instruction.
const CLOSE: [u8; 3] = [0x0f, 0x01, 0xca];

// Each site is a `nopl (%rax)` in text; the linker script lists their
// addresses.
unsafe extern "C" {
    /// The open sites' addresses.
    safe static smap_open_start: u8;
    /// Past the open sites' addresses.
    safe static smap_open_end: u8;
    /// The close sites' addresses.
    safe static smap_close_start: u8;
    /// Past the close sites' addresses.
    safe static smap_close_end: u8;
}

/// Where the CPU has SMAP, makes the open sites `stac` and the close sites
/// `clac`, then sets CR4.SMAP; clears it otherwise.
///
/// # Safety
///
/// Runs once, on the boot CPU, after `phys_map::bootstrap` and before any
/// site runs.
pub unsafe fn init() {
    let has_smap = cpuid::cpu_id()
        .get_extended_feature_info()
        .is_some_and(|info| info.has_smap());
    if has_smap {
        let lists = [
            (&raw const smap_open_start..&raw const smap_open_end, OPEN),
            (
                &raw const smap_close_start..&raw const smap_close_end,
                CLOSE,
            ),
        ];
        for (list, instruction) in lists {
            let count = (list.end.addr() - list.start.addr()) / size_of::<usize>();
            let start = ptr::with_exposed_provenance::<usize>(list.start.addr());
            // SAFETY: the list holds `count` addresses.
            for &site in unsafe { slice::from_raw_parts(start, count) } {
                // Text is read-only in the kernel window.
                let site = PhysAddr(site - KERNEL_BASE).direct_map_ptr::<[u8; 3]>();
                // SAFETY: a site's bytes, which no CPU runs yet.
                unsafe { site.write(instruction) };
            }
        }
    }
    // SAFETY: the kernel reaches user pages only between open and close
    // sites. The CR4 write serializes: no stale site runs past it.
    unsafe {
        Cr4::update(|flags| flags.set(Cr4Flags::SUPERVISOR_MODE_ACCESS_PREVENTION, has_smap));
    }
}
