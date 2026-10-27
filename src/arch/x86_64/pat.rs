// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The page attribute table, like dfly's `pmap_init_pat`: the memory type
//! each PAT, PCD and PWT combination in a page table entry selects.

use core::arch::x86_64::__cpuid;

use super::instructions::{self, IA32_PAT};

/// CPUID.01H:EDX.PAT.
const LEAF_1_EDX_PAT: u32 = 1 << 16;

/// CR0.NW: not write-through.
const CR0_NW: u64 = 1 << 29;
/// CR0.CD: cache disable.
const CR0_CD: u64 = 1 << 30;
/// CR4.PGE: global pages.
const CR4_PGE: u64 = 1 << 7;

/// Memory type encodings (Intel SDM, "PAT Memory Types").
const UNCACHEABLE: u8 = 0x00;
const WRITE_COMBINING: u8 = 0x01;
const WRITE_THROUGH: u8 = 0x04;
const WRITE_PROTECTED: u8 = 0x05;
const WRITE_BACK: u8 = 0x06;
/// UC-: uncacheable, unless an MTRR says write-combining.
const UNCACHED: u8 = 0x07;

/// Each entry's type, as dfly's: 0-3 as at reset, so entries without the PAT
/// bit keep their meaning, then WB, WP, WC and UC.
const LAYOUT: [u8; 8] = [
    WRITE_BACK,
    WRITE_THROUGH,
    UNCACHED,
    UNCACHEABLE,
    WRITE_BACK,
    WRITE_PROTECTED,
    WRITE_COMBINING,
    UNCACHEABLE,
];

/// Loads `LAYOUT` into this CPU's PAT, if it has one, with caches and TLB
/// flushed around the change, as dfly does; runs before `cpu::identify`.
pub fn init() {
    if __cpuid(1).edx & LEAF_1_EDX_PAT == 0 {
        return;
    }
    let cr0 = instructions::read_cr0();
    let cr4 = instructions::read_cr4();
    // SAFETY: the CPU has the PAT; caches and global pages are off only
    // meanwhile, and CR3 is reloaded with its own value.
    unsafe {
        instructions::write_cr4(cr4 & !CR4_PGE);
        instructions::write_cr0((cr0 & !CR0_NW) | CR0_CD);
        instructions::write_back_and_invalidate_cache();
        instructions::write_cr3(instructions::read_cr3());
        instructions::write_msr(IA32_PAT, u64::from_le_bytes(LAYOUT));
        instructions::write_back_and_invalidate_cache();
        instructions::write_cr3(instructions::read_cr3());
        instructions::write_cr0(cr0);
        instructions::write_cr4(cr4);
    }
}
