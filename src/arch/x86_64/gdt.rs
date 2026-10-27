// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Global descriptor tables, one per CPU.

use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use super::instructions::{self, PseudoDescriptor};
use super::tss::Tss;
use crate::smp::PerCpu;

/// Kernel code descriptor index.
pub const KERNEL_CODE_INDEX: u16 = 1;
/// Kernel data descriptor index.
pub const KERNEL_DATA_INDEX: u16 = 2;
/// TSS descriptor index; it takes two entries.
pub const TSS_INDEX: u16 = 6;

/// Kernel privilege level.
pub const KERNEL_PRIVILEGE: u16 = 0;

/// Selector for descriptor `index` at `privilege` (dfly `GSEL`).
pub const fn selector(index: u16, privilege: u16) -> u16 {
    index << 3 | privilege
}

/// A GDT; entries are atomic because the CPU writes them (accessed and busy bits).
#[repr(C)]
pub struct Gdt([AtomicU64; 9]);

impl Gdt {
    /// dfly's layout; accessed bits preset, the TSS filled by `set_tss`.
    pub const fn new() -> Self {
        Self([
            AtomicU64::new(0),
            AtomicU64::new(0x00af_9b00_0000_ffff), // KERNEL_CODE_INDEX: 64-bit code, ring 0
            AtomicU64::new(0x00cf_9300_0000_ffff), // KERNEL_DATA_INDEX: data, ring 0
            AtomicU64::new(0x00cf_fb00_0000_ffff), // 3: 32-bit code, ring 3
            AtomicU64::new(0x00cf_f300_0000_ffff), // 4: data, ring 3
            AtomicU64::new(0x00af_fb00_0000_ffff), // 5: 64-bit code, ring 3
            AtomicU64::new(0),                     // TSS_INDEX
            AtomicU64::new(0),                     // TSS_INDEX + 1
            AtomicU64::new(0x00cf_f300_0000_ffff), // 8: 32-bit GS, ring 3
        ])
    }

    /// Puts an available 64-bit TSS descriptor (type 0x9) for `tss` at
    /// `TSS_INDEX`.
    pub fn set_tss(&self, tss: &'static Tss) {
        let base = u64::try_from(ptr::from_ref(tss).expose_provenance()).unwrap();
        let limit = u64::try_from(size_of::<Tss>() - 1).unwrap();
        let index = usize::from(TSS_INDEX);
        self.0[index].store(
            limit | (base & 0xff_ffff) << 16 | 0x89 << 40 | (base & 0xff00_0000) << 32,
            Ordering::Relaxed,
        );
        self.0[index + 1].store(base >> 32, Ordering::Relaxed);
    }

    /// Loads this GDT and reloads every segment register.
    ///
    /// # Safety
    ///
    /// No interrupt may arrive meanwhile.
    pub unsafe fn load(&'static self) {
        let descriptor = PseudoDescriptor {
            limit: u16::try_from(size_of_val(self) - 1).unwrap(),
            base: ptr::from_ref(self).cast(),
        };
        // SAFETY: `self` is static and holds kernel code and data at their
        // indices; the caller keeps interrupts away.
        unsafe { instructions::load_gdt(&descriptor) };
    }
}

/// Loads `per_cpu`'s GDT and reloads the segment registers.
pub fn init(per_cpu: &'static PerCpu) {
    // SAFETY: interrupts are off.
    unsafe { per_cpu.md.gdt.load() };
}
