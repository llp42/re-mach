// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Multiboot2 header and 32-bit entry: maps the kernel and enters long mode.

use core::arch::global_asm;

use x86_64::registers::control::{Cr0Flags, Cr4Flags};
use x86_64::registers::model_specific::EferFlags;
use x86_64::structures::gdt::DescriptorFlags;
use x86_64::structures::paging::PageTableFlags;

use super::boot_stack;
use super::memory_layout::MIN_KERNEL_ADDRESS;
use crate::boot::multiboot2::{ARCHITECTURE_X86, HEADER_MAGIC, HeaderTagType, TagType};

/// EFER's MSR number.
const MSR_EFER: u32 = 0xc000_0080;

/// Boot GDT selectors.
const BOOT_CS: u16 = 0x8;
const BOOT_DS: u16 = 0x10;

/// Top-level entry, and next-level entry, of the kernel map base.
const KERNEL_P4: u64 = (MIN_KERNEL_ADDRESS >> 39) & 0x1ff;
const KERNEL_P3: u64 = (MIN_KERNEL_ADDRESS >> 30) & 0x1ff;

/// Page directory or table entry.
const PTE: u64 = PageTableFlags::PRESENT
    .union(PageTableFlags::WRITABLE)
    .bits();

/// 2 MiB page entry.
const PTE_2M: u64 = PTE | PageTableFlags::HUGE_PAGE.bits();

// A Multiboot2 loader enters `boot_start` in 32-bit protected mode, `%eax`
// holding its magic and `%ebx` the boot information's physical address. The
// first 4 GiB are identity-mapped and the first 2 GiB mapped again at the
// kernel map base, where `init` runs.
global_asm!(
    ".pushsection .boot.text, \"ax\"",
    ".globl boot_start",
    "boot_start:",
    ".code32",
    "jmp boot_entry",
    // The header and its tags are 8-byte aligned.
    ".balign 8",
    "multiboot2_hdr:",
    ".long {header_magic}",
    ".long {architecture}",
    ".long multiboot2_hdr_end - multiboot2_hdr",
    ".long -({header_magic} + {architecture} + (multiboot2_hdr_end - multiboot2_hdr))",
    // The boot information must hold a memory map.
    ".short {information_request}, 0",
    ".long 12",
    ".long {mmap}",
    ".balign 8, 0",
    ".short {end}, 0",
    ".long 8",
    "multiboot2_hdr_end:",
    "boot_entry:",
    "movl %eax, %edi",
    // Identity map: four directories of 2 MiB pages.
    "movl $p3table, %eax",
    "orl ${pte}, %eax",
    "movl %eax, p4table",
    "movl $p2table, %eax",
    "orl ${pte}, %eax",
    "movl %eax, p3table",
    "movl $p2table1, %eax",
    "orl ${pte}, %eax",
    "movl %eax, p3table + 8",
    "movl $p2table2, %eax",
    "orl ${pte}, %eax",
    "movl %eax, p3table + 16",
    "movl $p2table3, %eax",
    "orl ${pte}, %eax",
    "movl %eax, p3table + 24",
    "xorl %ecx, %ecx",
    "1:",
    "movl $0x200000, %eax",
    "mull %ecx",
    "orl ${pte_2m}, %eax",
    "movl %eax, p2table(, %ecx, 8)",
    "incl %ecx",
    "cmpl $2048, %ecx",
    "jne 1b",
    // Kernel map: two directories at the kernel map base.
    "movl $p3ktable, %eax",
    "orl ${pte}, %eax",
    "movl %eax, p4table + 8 * {kernel_p4}",
    "movl $p2ktable1, %eax",
    "orl ${pte}, %eax",
    "movl %eax, p3ktable + 8 * {kernel_p3}",
    "movl $p2ktable2, %eax",
    "orl ${pte}, %eax",
    "movl %eax, p3ktable + 8 * ({kernel_p3} + 1)",
    "xorl %ecx, %ecx",
    "2:",
    "movl $0x200000, %eax",
    "mull %ecx",
    "orl ${pte_2m}, %eax",
    "movl %eax, p2ktable1(, %ecx, 8)",
    "incl %ecx",
    "cmpl $1024, %ecx",
    "jne 2b",
    // PAE, long mode, paging, then a 64-bit code segment.
    "movl %cr4, %eax",
    "orl ${cr4_pae}, %eax",
    "movl %eax, %cr4",
    "movl ${msr_efer}, %ecx",
    "rdmsr",
    "orl ${efer_lme}, %eax",
    "wrmsr",
    "movl $p4table, %eax",
    "movl %eax, %cr3",
    "movl %cr0, %eax",
    "orl ${cr0_pg_wp}, %eax",
    "movl %eax, %cr0",
    "lgdt gdt64pointer",
    "ljmp ${boot_cs}, $boot_entry64",
    ".code64",
    "boot_entry64:",
    "xorl %eax, %eax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movw %ax, %fs",
    "movw %ax, %gs",
    "movw ${boot_ds}, %ax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movabsq ${stack} + {stack_top}, %rsp",
    "movl %edi, %esi",
    "movl %ebx, %edi",
    "movabsq ${init}, %rax",
    "callq *%rax",
    ".popsection",
    ".pushsection .boot.data, \"aw\"",
    ".balign 8",
    "gdt64:",
    ".quad 0, {code64}, {data}",
    "gdt64end:",
    "gdt64pointer:",
    ".short gdt64end - gdt64 - 1",
    ".quad gdt64",
    ".balign 4096",
    "p4table: .space 4096",
    "p3table: .space 4096",
    "p2table: .space 4096",
    "p2table1: .space 4096",
    "p2table2: .space 4096",
    "p2table3: .space 4096",
    "p3ktable: .space 4096",
    "p2ktable1: .space 4096",
    "p2ktable2: .space 4096",
    ".popsection",
    header_magic = const HEADER_MAGIC,
    architecture = const ARCHITECTURE_X86,
    information_request = const HeaderTagType::INFORMATION_REQUEST.0,
    mmap = const TagType::MMAP.0,
    end = const HeaderTagType::END.0,
    pte = const PTE,
    pte_2m = const PTE_2M,
    kernel_p4 = const KERNEL_P4,
    kernel_p3 = const KERNEL_P3,
    cr4_pae = const Cr4Flags::PHYSICAL_ADDRESS_EXTENSION.bits(),
    msr_efer = const MSR_EFER,
    efer_lme = const EferFlags::LONG_MODE_ENABLE.bits(),
    cr0_pg_wp = const Cr0Flags::PAGING.union(Cr0Flags::WRITE_PROTECT).bits(),
    boot_cs = const BOOT_CS,
    boot_ds = const BOOT_DS,
    stack = sym boot_stack::STACK,
    stack_top = const boot_stack::SIZE - 16,
    init = sym super::init,
    code64 = const DescriptorFlags::KERNEL_CODE64.bits(),
    data = const DescriptorFlags::KERNEL_DATA.bits(),
    options(att_syntax),
);
