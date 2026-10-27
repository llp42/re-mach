// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Global descriptor tables, one per CPU.

use core::ptr;
use core::sync::atomic::AtomicU64;

use super::instructions::{self, PseudoDescriptor};
use crate::smp::PerCpu;

/// Kernel code descriptor index.
pub const KERNEL_CODE_INDEX: u16 = 1;
/// Kernel data descriptor index.
pub const KERNEL_DATA_INDEX: u16 = 2;

/// Kernel privilege level.
pub const KERNEL_PRIVILEGE: u16 = 0;

/// Selector for descriptor `index` at `privilege` (dfly `GSEL`).
pub const fn selector(index: u16, privilege: u16) -> u16 {
    index << 3 | privilege
}

/// A GDT; entries are atomic because the CPU writes them (accessed and busy bits).
#[repr(C)]
pub struct Gdt([AtomicU64; 3]);

impl Gdt {
    /// Null, kernel code, kernel data; accessed bits preset.
    pub const fn new() -> Self {
        Self([
            AtomicU64::new(0),
            AtomicU64::new(0x00af_9b00_0000_ffff), // KERNEL_CODE_INDEX: 64-bit code, ring 0
            AtomicU64::new(0x00cf_9300_0000_ffff), // KERNEL_DATA_INDEX: data, ring 0
        ])
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
