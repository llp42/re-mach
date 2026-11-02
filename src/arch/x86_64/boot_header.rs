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
use super::memory_layout::KERNEL_BASE;
use crate::boot::multiboot2::{ARCHITECTURE_X86, HEADER_MAGIC, HeaderTagType, TagType};

/// The EFER's MSR number.
const IA32_EFER: u32 = 0xc000_0080;

/// Boot GDT selectors.
const BOOT_CODE_SELECTOR: u16 = 0x8;
const BOOT_DATA_SELECTOR: u16 = 0x10;

/// The kernel map base's PML4 and PDPT indexes.
const KERNEL_PML4_INDEX: usize = (KERNEL_BASE >> 39) & 0x1ff;
const KERNEL_PDPT_INDEX: usize = (KERNEL_BASE >> 30) & 0x1ff;

/// Flags of an entry pointing at a lower table.
const TABLE_ENTRY: u64 = PageTableFlags::PRESENT
    .union(PageTableFlags::WRITABLE)
    .bits();

/// Flags of a 2 MiB page entry.
const LARGE_PAGE_ENTRY: u64 = TABLE_ENTRY | PageTableFlags::HUGE_PAGE.bits();

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
    "orl ${table_entry}, %eax",
    "movl %eax, p4table",
    "movl $p2table, %eax",
    "orl ${table_entry}, %eax",
    "movl %eax, p3table",
    "movl $p2table1, %eax",
    "orl ${table_entry}, %eax",
    "movl %eax, p3table + 8",
    "movl $p2table2, %eax",
    "orl ${table_entry}, %eax",
    "movl %eax, p3table + 16",
    "movl $p2table3, %eax",
    "orl ${table_entry}, %eax",
    "movl %eax, p3table + 24",
    "xorl %ecx, %ecx",
    "1:",
    "movl $0x200000, %eax",
    "mull %ecx",
    "orl ${large_page_entry}, %eax",
    "movl %eax, p2table(, %ecx, 8)",
    "incl %ecx",
    "cmpl $2048, %ecx",
    "jne 1b",
    // Kernel map: two directories at the kernel map base.
    "movl $p3ktable, %eax",
    "orl ${table_entry}, %eax",
    "movl %eax, p4table + 8 * {kernel_pml4_index}",
    "movl $p2ktable1, %eax",
    "orl ${table_entry}, %eax",
    "movl %eax, p3ktable + 8 * {kernel_pdpt_index}",
    "movl $p2ktable2, %eax",
    "orl ${table_entry}, %eax",
    "movl %eax, p3ktable + 8 * ({kernel_pdpt_index} + 1)",
    "xorl %ecx, %ecx",
    "2:",
    "movl $0x200000, %eax",
    "mull %ecx",
    "orl ${large_page_entry}, %eax",
    "movl %eax, p2ktable1(, %ecx, 8)",
    "incl %ecx",
    "cmpl $1024, %ecx",
    "jne 2b",
    // PAE, long mode, paging, then a 64-bit code segment.
    "movl %cr4, %eax",
    "orl ${cr4_pae}, %eax",
    "movl %eax, %cr4",
    "movl ${ia32_efer}, %ecx",
    "rdmsr",
    "orl ${efer_lme}, %eax",
    "wrmsr",
    "movl $p4table, %eax",
    "movl %eax, %cr3",
    "movl %cr0, %eax",
    "orl ${cr0_pg_wp}, %eax",
    "movl %eax, %cr0",
    "lgdt gdt64pointer",
    "ljmp ${boot_code_selector}, $boot_entry64",
    ".code64",
    "boot_entry64:",
    "xorl %eax, %eax",
    "movw %ax, %ds",
    "movw %ax, %es",
    "movw %ax, %ss",
    "movw %ax, %fs",
    "movw %ax, %gs",
    "movw ${boot_data_selector}, %ax",
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
    table_entry = const TABLE_ENTRY,
    large_page_entry = const LARGE_PAGE_ENTRY,
    kernel_pml4_index = const KERNEL_PML4_INDEX,
    kernel_pdpt_index = const KERNEL_PDPT_INDEX,
    cr4_pae = const Cr4Flags::PHYSICAL_ADDRESS_EXTENSION.bits(),
    ia32_efer = const IA32_EFER,
    efer_lme = const EferFlags::LONG_MODE_ENABLE.bits(),
    cr0_pg_wp = const Cr0Flags::PAGING.union(Cr0Flags::WRITE_PROTECT).bits(),
    boot_code_selector = const BOOT_CODE_SELECTOR,
    boot_data_selector = const BOOT_DATA_SELECTOR,
    stack = sym boot_stack::STACK,
    stack_top = const boot_stack::SIZE - 16,
    init = sym super::init,
    code64 = const DescriptorFlags::KERNEL_CODE64.bits(),
    data = const DescriptorFlags::KERNEL_DATA.bits(),
    options(att_syntax),
);
