// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! CPU identification and setup, like dfly's `identify_cpu` and `initializecpu`.

use core::arch::x86_64::{__cpuid, __cpuid_count};
use core::fmt::{self, Write};
use core::str;
use core::sync::atomic::{AtomicU32, Ordering};

use super::instructions::{self, IA32_EFER};
use super::serial::Serial;
use crate::smp::PerCpu;

/// CPUID.01H:EDX.FXSR: `fxsave`/`fxrstor`.
const LEAF_1_EDX_FXSR: u32 = 1 << 24;
/// CPUID.01H:EDX.SSE.
const LEAF_1_EDX_SSE: u32 = 1 << 25;
/// CPUID.80000001H:EDX.NX: no-execute pages.
pub(super) const EXTENDED_EDX_NX: u32 = 1 << 20;

/// CR4.OSFXSR: the OS saves SSE state with `fxsave`.
const CR4_OSFXSR: u64 = 1 << 9;
/// CR4.OSXMMEXCPT: the OS handles SIMD exceptions (#XM).
const CR4_OSXMMEXCPT: u64 = 1 << 10;
/// `IA32_EFER.NXE`: no-execute pages.
pub(super) const EFER_NXE: u64 = 1 << 11;

/// The CPUID results the kernel keeps; atomic, so they're set through `&self`.
pub struct Cpuid {
    /// Leaf 0 EBX, EDX, ECX: the vendor string.
    vendor: [AtomicU32; 3],
    /// Leaf 1 EAX: stepping, model, family.
    signature: AtomicU32,
    leaf_1_ecx: AtomicU32,
    leaf_1_edx: AtomicU32,
    /// Leaf 7, subleaf 0; zero if the CPU lacks it.
    leaf_7_ebx: AtomicU32,
    leaf_7_ecx: AtomicU32,
    /// Leaf 0x80000001; zero if the CPU lacks it.
    extended_ecx: AtomicU32,
    extended_edx: AtomicU32,
}

impl Cpuid {
    pub const fn new() -> Self {
        Self {
            vendor: [const { AtomicU32::new(0) }; 3],
            signature: AtomicU32::new(0),
            leaf_1_ecx: AtomicU32::new(0),
            leaf_1_edx: AtomicU32::new(0),
            leaf_7_ebx: AtomicU32::new(0),
            leaf_7_ecx: AtomicU32::new(0),
            extended_ecx: AtomicU32::new(0),
            extended_edx: AtomicU32::new(0),
        }
    }
}

impl fmt::Display for Cpuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut vendor = [0; 12];
        for (bytes, word) in vendor.as_chunks_mut::<4>().0.iter_mut().zip(&self.vendor) {
            *bytes = word.load(Ordering::Relaxed).to_le_bytes();
        }
        // Family and model as the Intel SDM defines them for CPUID.01H:EAX.
        let signature = self.signature.load(Ordering::Relaxed);
        let base_family = (signature >> 8) & 0xf;
        let family = if base_family == 0xf {
            base_family + ((signature >> 20) & 0xff)
        } else {
            base_family
        };
        let model = if base_family == 0x6 || base_family == 0xf {
            ((signature >> 12) & 0xf0) | ((signature >> 4) & 0xf)
        } else {
            (signature >> 4) & 0xf
        };
        write!(
            f,
            "{} family {:#x} model {:#x} stepping {} leaf 1 ecx {:#x} edx {:#x} \
             leaf 7 ebx {:#x} ecx {:#x} leaf 0x80000001 ecx {:#x} edx {:#x}",
            str::from_utf8(&vendor).unwrap_or("?"),
            family,
            model,
            signature & 0xf,
            self.leaf_1_ecx.load(Ordering::Relaxed),
            self.leaf_1_edx.load(Ordering::Relaxed),
            self.leaf_7_ebx.load(Ordering::Relaxed),
            self.leaf_7_ecx.load(Ordering::Relaxed),
            self.extended_ecx.load(Ordering::Relaxed),
            self.extended_edx.load(Ordering::Relaxed),
        )
    }
}

/// Reads this CPU's CPUID into `per_cpu` and prints it, as dfly's
/// `identify_cpu` and `printcpuinfo` do.
pub fn identify(per_cpu: &'static PerCpu) {
    let cpuid = &per_cpu.md.cpuid;
    let leaf_0 = __cpuid(0);
    for (word, value) in cpuid
        .vendor
        .iter()
        .zip([leaf_0.ebx, leaf_0.edx, leaf_0.ecx])
    {
        word.store(value, Ordering::Relaxed);
    }
    let leaf_1 = __cpuid(1);
    cpuid.signature.store(leaf_1.eax, Ordering::Relaxed);
    cpuid.leaf_1_ecx.store(leaf_1.ecx, Ordering::Relaxed);
    cpuid.leaf_1_edx.store(leaf_1.edx, Ordering::Relaxed);
    if leaf_0.eax >= 7 {
        let leaf_7 = __cpuid_count(7, 0);
        cpuid.leaf_7_ebx.store(leaf_7.ebx, Ordering::Relaxed);
        cpuid.leaf_7_ecx.store(leaf_7.ecx, Ordering::Relaxed);
    }
    if __cpuid(0x8000_0000).eax >= 0x8000_0001 {
        let extended = __cpuid(0x8000_0001);
        cpuid.extended_ecx.store(extended.ecx, Ordering::Relaxed);
        cpuid.extended_edx.store(extended.edx, Ordering::Relaxed);
    }
    let _ = write!(Serial, "cpu {cpuid}\r\n");
}

/// Turns on SSE and no-execute pages if the CPU has them, as dfly's
/// `initializecpu` does.
pub fn initialize(per_cpu: &'static PerCpu) {
    let cpuid = &per_cpu.md.cpuid;
    let sse = LEAF_1_EDX_FXSR | LEAF_1_EDX_SSE;
    if cpuid.leaf_1_edx.load(Ordering::Relaxed) & sse == sse {
        // SAFETY: the CPU has `fxsave` and SSE.
        unsafe { instructions::write_cr4(instructions::read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT) };
    }
    if cpuid.extended_edx.load(Ordering::Relaxed) & EXTENDED_EDX_NX != 0 {
        // SAFETY: the CPU has NX, so IA32_EFER.NXE exists.
        unsafe { instructions::write_msr(IA32_EFER, instructions::read_msr(IA32_EFER) | EFER_NXE) };
    }
}
