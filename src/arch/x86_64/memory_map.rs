// SPDX-License-Identifier: BSD-4-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from DragonFly BSD
// Upstream-Path: sys/platform/pc64/x86_64/machdep.c
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-4-Clause
// SPDX-FileCopyrightText: 1982, 1987, 1990 The Regents of the University of California.
// SPDX-FileCopyrightText: 1992 Terrence R. Lambert.
// SPDX-FileCopyrightText: 2003 Peter Wemm.
// SPDX-FileCopyrightText: 2008-2017 The DragonFly Project.
//! Physical memory ranges from the loader's BIOS memory map.
//!
//! The kernel's memory setup, `getmemsize`, calls this when the loader
//! passed a BIOS memory map rather than an EFI one. The map, the ranges and
//! the memory total are globals of that setup.

use core::ffi::c_int;

use crate::ffi;

unsafe extern "C" {
    /// Physical memory ranges as start/end pairs.
    #[link_name = "physmap"]
    static mut PHYSICAL_RANGES: [u64; 127];
    /// Data of the loader's memory map record: packed 20-byte entries of
    /// base, length and type, preceded by the record's 32-bit byte size.
    #[link_name = "smapbase"]
    static mut MAP_BASE: *const u8;
    /// The entry being read; after the last one, the end of the entries.
    #[link_name = "smap"]
    static mut MAP_ENTRY: *const u8;
    /// End of the memory map's data.
    #[link_name = "smapend"]
    static mut MAP_END: *const u8;
    /// Byte size of the memory map's data.
    #[link_name = "smapsize"]
    static mut MAP_SIZE: u32;
    /// Total size of the usable memory the map reports.
    #[link_name = "Realmem"]
    static mut REAL_MEMORY: u64;
    /// Boot flags.
    #[link_name = "boothowto"]
    static mut BOOT_HOWTO: c_int;
}

/// Adds the usable entries of the memory map at `smapbase` to `physmap`
/// and their sizes to `Realmem`, and sets `smapsize`, `smapend` and `smap`.
///
/// `physmap_idx` is the index of the last start/end pair in use. An entry
/// that starts below the end of a pair in use is skipped; one that starts
/// where the last pair ends extends it; any other one becomes a new pair
/// after it, and `physmap_idx` moves to that pair. Entries are printed when
/// booting verbosely.
///
/// Called by `getmemsize`, after the console is set up, with `smapbase` set
/// to the record's data and `physmap_idx` pointing to its own index.
///
/// # Panics
///
/// When a new pair would start at index 126: the check for a full
/// `physmap` compares the index, which is always even, with its odd length,
/// so it never stops the loop, and that pair's end lies outside `physmap`.
#[unsafe(no_mangle)]
pub extern "C" fn add_smap_entries(
    // TODO: check the possibility to change to u32 or usize in the future
    physmap_idx: &mut i32,
) {
    // SAFETY: getmemsize runs on the boot CPU alone with interrupts
    // disabled, so nothing else accesses these globals during the call. It
    // set smapbase to the memory map record's data, which the record's
    // 32-bit byte size precedes and which nothing writes while it is read.
    let (map, physmap, verbose) = unsafe {
        debug_assert!(!MAP_BASE.is_null());
        MAP_SIZE = MAP_BASE.cast::<u32>().sub(1).read_unaligned();
        MAP_END = MAP_BASE.add(MAP_SIZE as usize);
        (
            core::slice::from_raw_parts(MAP_BASE, MAP_SIZE as usize),
            &mut *(&raw mut PHYSICAL_RANGES),
            // 0x800: verbose boot.
            BOOT_HOWTO & 0x800 != 0,
        )
    };

    for entry in map.chunks_exact(20) {
        // SAFETY: as above, nothing else accesses smap.
        unsafe { MAP_ENTRY = entry.as_ptr() };
        // The ranges are 8, 8 and 4 bytes long, so the conversions succeed.
        let base = u64::from_le_bytes(entry[0..8].try_into().unwrap());
        let length = u64::from_le_bytes(entry[8..16].try_into().unwrap());
        let kind = u32::from_le_bytes(entry[16..20].try_into().unwrap());
        if verbose {
            // SAFETY: the format's conversions match the arguments.
            unsafe {
                ffi::kprintf(
                    c"SMAP type=%02x base=%016lx len=%016lx\n".as_ptr(),
                    kind,
                    base,
                    length,
                )
            };
        }

        // Type 1 is usable memory.
        if kind != 1 || length == 0 {
            continue;
        }

        if (0..=*physmap_idx)
            .step_by(2)
            .any(|i| base < physmap[(i + 1) as usize])
        {
            if verbose {
                // SAFETY: the format has no conversions.
                unsafe {
                    ffi::kprintf(
                        c"Overlapping or non-monotonic memory region, ignoring second region\n"
                            .as_ptr(),
                    )
                };
            }
            continue;
        }

        // SAFETY: as above, nothing else accesses Realmem.
        unsafe { REAL_MEMORY = REAL_MEMORY.wrapping_add(length) };

        // getmemsize starts with one empty pair at 0, which a region at 0
        // extends.
        let last_end = &mut physmap[(*physmap_idx + 1) as usize];
        if base == *last_end {
            *last_end = last_end.wrapping_add(length);
            continue;
        }

        *physmap_idx += 2;
        if *physmap_idx as usize == physmap.len() {
            // SAFETY: the format has no conversions.
            unsafe {
                ffi::kprintf(c"Too many segments in the physical address map, giving up\n".as_ptr())
            };
            return;
        }
        physmap[*physmap_idx as usize] = base;
        physmap[(*physmap_idx + 1) as usize] = base.wrapping_add(length);
    }
    // SAFETY: as above, nothing else accesses smap.
    unsafe { MAP_ENTRY = map.as_ptr_range().end };
}
