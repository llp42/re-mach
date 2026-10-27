// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Instruction wrappers, like dfly's `cpufunc.h`.

use core::arch::asm;

use super::gdt::{KERNEL_CODE_INDEX, KERNEL_DATA_INDEX, KERNEL_PRIVILEGE, selector};

/// `lgdt`/`lidt` operand.
#[repr(C, packed)]
pub struct PseudoDescriptor {
    /// Table size in bytes, minus one.
    pub limit: u16,
    pub base: *const (),
}

/// Extended features MSR (long mode, no-execute, `syscall`).
pub const IA32_EFER: u32 = 0xc000_0080;
/// FS base MSR.
pub const IA32_FS_BASE: u32 = 0xc000_0100;
/// GS base MSR.
pub const IA32_GS_BASE: u32 = 0xc000_0101;
/// The GS base `swapgs` exchanges with GS's.
pub const IA32_KERNEL_GS_BASE: u32 = 0xc000_0102;
/// Page attribute table MSR.
pub const IA32_PAT: u32 = 0x277;

/// Loads the GDT, then DS, ES, FS, GS and SS with kernel data and CS with
/// kernel code, as dfly's `lgdt` does.
///
/// # Safety
///
/// `descriptor` describes a GDT that stays valid, with kernel code and data at
/// their indices; no interrupt may arrive meanwhile.
pub unsafe fn load_gdt(descriptor: &PseudoDescriptor) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        asm!(
            "lgdt ({descriptor})",
            "mov {data:e}, %ds",
            "mov {data:e}, %es",
            "mov {data:e}, %fs",
            "mov {data:e}, %gs",
            "mov {data:e}, %ss",
            "pushq ${code}",
            "lea 2f(%rip), {return_address}",
            "push {return_address}",
            "lretq",
            "2:",
            descriptor = in(reg) descriptor,
            data = in(reg) u32::from(selector(KERNEL_DATA_INDEX, KERNEL_PRIVILEGE)),
            code = const selector(KERNEL_CODE_INDEX, KERNEL_PRIVILEGE),
            return_address = out(reg) _,
            options(preserves_flags, att_syntax),
        );
    }
}

/// Loads the IDT.
///
/// # Safety
///
/// `descriptor` describes an IDT that stays valid, whose present gates point
/// to interrupt handlers.
pub unsafe fn load_idt(descriptor: &PseudoDescriptor) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        asm!("lidt ({descriptor})", descriptor = in(reg) descriptor, options(readonly, nostack, preserves_flags, att_syntax));
    }
}

/// Reads `msr`.
///
/// # Safety
///
/// Reading an MSR the CPU doesn't have raises #GP.
pub unsafe fn read_msr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: guaranteed by the caller.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags, att_syntax));
    }
    u64::from(high) << 32 | u64::from(low)
}

/// Loads the task register.
///
/// # Safety
///
/// `selector` names an available TSS descriptor in the loaded GDT, and that
/// TSS stays valid.
pub unsafe fn load_task_register(selector: u16) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        asm!("ltr {0:x}", in(reg) selector, options(nostack, preserves_flags, att_syntax));
    }
}

/// Writes `value` to `msr`.
///
/// # Safety
///
/// MSR writes can change any CPU behavior.
pub unsafe fn write_msr(msr: u32, value: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        asm!(
            "mov %rax, %rdx",
            "shr $32, %rdx",
            "wrmsr",
            in("ecx") msr,
            inout("rax") value => _,
            out("rdx") _,
            options(nostack, att_syntax),
        );
    }
}

/// Reads CR0.
pub fn read_cr0() -> u64 {
    let value;
    // SAFETY: reading CR0 has no side effects.
    unsafe {
        asm!("mov %cr0, {}", out(reg) value, options(nomem, nostack, preserves_flags, att_syntax));
    }
    value
}

/// Writes CR0.
///
/// # Safety
///
/// CR0 controls protection, paging and caching; a wrong value faults or
/// changes behavior anywhere.
pub unsafe fn write_cr0(value: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        asm!("mov {}, %cr0", in(reg) value, options(nostack, preserves_flags, att_syntax));
    }
}

/// Reads CR4.
pub fn read_cr4() -> u64 {
    let value;
    // SAFETY: reading CR4 has no side effects.
    unsafe {
        asm!("mov %cr4, {}", out(reg) value, options(nomem, nostack, preserves_flags, att_syntax));
    }
    value
}

/// Writes CR4.
///
/// # Safety
///
/// CR4 controls paging and CPU features; a wrong value faults or changes
/// behavior anywhere.
pub unsafe fn write_cr4(value: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        asm!("mov {}, %cr4", in(reg) value, options(nostack, preserves_flags, att_syntax));
    }
}

/// Reads CR3: the PML4's physical address and its flags.
pub fn read_cr3() -> u64 {
    let value;
    // SAFETY: reading CR3 has no side effects.
    unsafe {
        asm!("mov %cr3, {}", out(reg) value, options(nomem, nostack, preserves_flags, att_syntax));
    }
    value
}

/// Writes CR3, switching page tables.
///
/// # Safety
///
/// `value` is the physical address of a PML4 that maps the running code, its
/// stack and the data it uses.
pub unsafe fn write_cr3(value: u64) {
    // SAFETY: guaranteed by the caller.
    unsafe {
        asm!("mov {}, %cr3", in(reg) value, options(nostack, preserves_flags, att_syntax));
    }
}

/// Reads CR2, the last page-fault address.
pub fn read_cr2() -> u64 {
    let value;
    // SAFETY: reading CR2 has no side effects.
    unsafe {
        asm!("mov %cr2, {}", out(reg) value, options(nomem, nostack, preserves_flags, att_syntax));
    }
    value
}

/// Writes back every modified cache line to memory and invalidates the
/// caches (`wbinvd`).
pub fn write_back_and_invalidate_cache() {
    // SAFETY: memory keeps its contents; only the caches empty.
    unsafe {
        asm!("wbinvd", options(nostack, preserves_flags, att_syntax));
    }
}
