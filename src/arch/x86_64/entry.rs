// SPDX-License-Identifier: BSD-2-Clause AND BSD-3-Clause AND BSD-4-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from DragonFly BSD
// Upstream-Path: stand/boot/efi/loader/arch/x86_64/elf64_freebsd.c
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 1998 Michael Smith <msmith@freebsd.org>
// SPDX-FileCopyrightText: 2014 The FreeBSD Foundation
//
// Derived from DragonFly BSD
// Upstream-Path: stand/boot/efi/loader/arch/x86_64/amd64_tramp.S
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 2013 The FreeBSD Foundation
//
// Derived from DragonFly BSD
// Upstream-Path: sys/platform/pc64/include/vmparam.h
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-4-Clause
// SPDX-FileCopyrightText: 1990 The Regents of the University of California.
// SPDX-FileCopyrightText: 1994 John S. Dyson
// SPDX-FileCopyrightText: 2003 Peter Wemm
// SPDX-FileCopyrightText: 2008 The DragonFly Project.
//
// Derived from DragonFly BSD
// Upstream-Path: sys/platform/pc64/conf/ldscript.x86_64
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2003-2026 The DragonFly Project
//! Kernel entry from a Multiboot loader.
//!
//! The loader finds the Multiboot header in the image's first 8 KiB, loads
//! the file at `LOAD_ADDRESS`, zeroes it up to the image's end, and jumps to
//! the 32-bit entry with paging off and the information address in `%ebx`.
//! The image is linked so that each segment belongs at `LOAD_ADDRESS` plus
//! its file offset; the boot code checks this before relying on it.
//!
//! The entry turns on long mode under static page tables that map physical
//! `0..KERNEL_LIMIT` both at the same addresses and at `KERNEL_BASE`, and
//! calls the boot code at its linked address. The tables and descriptors
//! live in read-only data, which shares the first segment with the entry and
//! so is in place even when a later segment is not. The boot code enters
//! the kernel through `enter_kernel`.

use core::arch::{asm, global_asm};

/// Virtual address of physical address zero in the kernel.
pub const KERNEL_BASE: u64 = 0xffff_ffff_8000_0000;
/// Physical address of the kernel image's first byte.
pub const LOAD_ADDRESS: u64 = 0x20_0000;
/// End of the physical range the entry page tables map.
pub const KERNEL_LIMIT: u64 = 1 << 30;

const MULTIBOOT_MAGIC: u32 = 0x1bad_b002;
/// Page-aligned modules, memory information, and the load addresses in the
/// header, which let the loader place a 64-bit image.
const MULTIBOOT_FLAGS: u32 = 1 << 0 | 1 << 1 | 1 << 16;
/// What the loader leaves in `%eax`.
const LOADER_MAGIC: u32 = 0x2bad_b002;
const BOOT_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct BootStack([u8; BOOT_STACK_SIZE]);

/// The boot code's stack, until the kernel switches to its own.
static mut BOOT_STACK: BootStack = BootStack([0; BOOT_STACK_SIZE]);

global_asm!(
    ".pushsection .multiboot, \"a\"",
    ".balign 4",
    "multiboot_header:",
    ".long {magic}",
    ".long {flags}",
    ".long -({magic} + {flags})",
    ".long multiboot_header - {base}",
    ".long {load}",
    ".long kernel_load_end - {base}",
    ".long end - {base}",
    ".long multiboot_entry - {base}",
    ".popsection",
    //
    ".pushsection .text.multiboot_entry, \"ax\", @progbits",
    ".code32",
    "multiboot_entry:",
    "cli",
    "cmpl ${loader_magic}, %eax",
    "jne 3f",
    "movl %ebx, %edi",
    // Physical address extension, the entry page tables, long mode, paging.
    "movl %cr4, %eax",
    "orl $0x20, %eax",
    "movl %eax, %cr4",
    "movl $(entry_pml4 - {base}), %eax",
    "movl %eax, %cr3",
    "movl $0xc0000080, %ecx",
    "rdmsr",
    "orl $0x100, %eax",
    "wrmsr",
    "lgdt (entry_gdt_pointer - {base})",
    "movl %cr0, %eax",
    "orl $0x80000001, %eax",
    "movl %eax, %cr0",
    "ljmp $0x08, $(entry_long_mode - {base})",
    "3:",
    "hlt",
    "jmp 3b",
    ".code64",
    "entry_long_mode:",
    "movw $0x10, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movw %ax, %fs",
    "movw %ax, %gs",
    // The upper halves of the registers are undefined after the switch.
    "movl %edi, %edi",
    "movabsq ${stack} + {stack_size}, %rsp",
    "movabsq ${boot}, %rax",
    "callq *%rax",
    "4:",
    "hlt",
    "jmp 4b",
    ".popsection",
    //
    // PML4[0] and PML4[511] -> PDPT[510] both reach one page directory of
    // 2 MiB pages over 0..KERNEL_LIMIT: present, writable, large.
    ".pushsection .rodata.multiboot_entry, \"a\"",
    ".balign 4096",
    "entry_pml4:",
    ".quad entry_low_pdpt - {base} + 3",
    ".fill 510, 8, 0",
    ".quad entry_high_pdpt - {base} + 3",
    "entry_low_pdpt:",
    ".quad entry_pd - {base} + 3",
    ".fill 511, 8, 0",
    "entry_high_pdpt:",
    ".fill 510, 8, 0",
    ".quad entry_pd - {base} + 3",
    ".quad 0",
    "entry_pd:",
    ".set entry_page, 0",
    ".rept 512",
    ".quad entry_page * 0x200000 + 0x83",
    ".set entry_page, entry_page + 1",
    ".endr",
    // Null, 64-bit code and data descriptors.
    "entry_gdt:",
    ".quad 0",
    ".quad 0x00af9a000000ffff",
    ".quad 0x00cf92000000ffff",
    "entry_gdt_pointer:",
    ".word entry_gdt_pointer - entry_gdt - 1",
    ".long entry_gdt - {base}",
    ".popsection",
    magic = const MULTIBOOT_MAGIC,
    flags = const MULTIBOOT_FLAGS,
    loader_magic = const LOADER_MAGIC,
    base = const KERNEL_BASE,
    load = const LOAD_ADDRESS,
    stack = sym BOOT_STACK,
    stack_size = const BOOT_STACK_SIZE,
    boot = sym crate::boot::multiboot::enter,
    options(att_syntax),
);

/// Jumps to the kernel's entry point on a fresh boot stack holding
/// `module_info` and `kernel_end` where the kernel reads them; interrupts
/// stay disabled.
///
/// # Safety
///
/// - The entry page tables are active.
/// - The image, environment and metadata are complete, with the metadata
///   at `module_info`.
/// - Nothing still in use lies from `kernel_end` on, which the kernel's
///   first page tables overwrite.
pub unsafe fn enter_kernel(module_info: u32, kernel_end: u32) -> ! {
    unsafe extern "C" {
        fn btext();
    }
    let stack_top = (&raw const BOOT_STACK) as u64 + BOOT_STACK_SIZE as u64;
    // The kernel reads the 32-bit module_info at 4(%rsp), above an unused
    // return address, and kernel_end at 8(%rsp).
    // SAFETY: the caller's contract; control does not return, so the boot
    // stack can be reset.
    unsafe {
        asm!(
            "mov {stack}, %rsp",
            "push {kernel_end}",
            "push {module_info}",
            "jmp {btext}",
            stack = in(reg) stack_top,
            kernel_end = in(reg) u64::from(kernel_end),
            module_info = in(reg) u64::from(module_info) << 32,
            btext = sym btext,
            options(noreturn, att_syntax),
        )
    }
}
