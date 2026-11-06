// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Machine-dependent virtual memory parameters.

use super::address::PhysAddr;

/// Bytes per page.
pub const PAGE_SIZE: usize = 4096;

/// Bytes of a thread's kernel stack.
pub const KERNEL_STACK_SIZE: usize = 16 << 10;

/// Start of user space.
pub const MIN_USER_ADDRESS: usize = 0;

/// Past the last user address.
pub const MAX_USER_ADDRESS: usize = 0x8000_0000_0000;

/// The PML4 maps itself here, showing every page table.
pub const PAGE_TABLE_BASE: usize = 0xffff_8000_0000_0000;

/// Physical memory is mapped in full from here, across 128 PML4 slots.
pub const DIRECT_MAP_BASE: usize = 0xffff_b800_0000_0000;

/// Start of kernel virtual space, its last 16 PML4 slots.
pub const MIN_KERNEL_ADDRESS: usize = 0xffff_f800_0000_0000;

/// Base of the kernel window, where the kernel image runs.
pub const KERNEL_BASE: usize = 0xffff_ffff_8000_0000;

/// Last of the kernel's virtual addresses.
pub const MAX_KERNEL_ADDRESS: usize = 0xffff_ffff_ffff_ffff;

/// Kernel map size; the rest of the kernel window maps physical memory directly.
pub const KERNEL_MAP_SIZE: usize = 1024 * 1024 * 1024;

/// End of the memory ISA DMA reaches.
pub const DMA_LIMIT: PhysAddr = PhysAddr(0x100_0000);

/// End of the memory 32-bit DMA reaches.
pub const DMA32_LIMIT: PhysAddr = PhysAddr(0x1_0000_0000);

/// End of the physical memory mapped directly in the kernel's space.
pub const DIRECT_MAP_LIMIT: PhysAddr =
    PhysAddr(MAX_KERNEL_ADDRESS - KERNEL_BASE - KERNEL_MAP_SIZE + 1);

/// End of the 52-bit physical address space.
pub const HIGH_MEMORY_LIMIT: PhysAddr = PhysAddr(0x10_0000_0000_0000);

/// Physical segments, in address order: the direct map ends below 4 GiB.
#[derive(Clone, Copy)]
pub enum Segment {
    Dma,
    DirectMap,
    Dma32,
    HighMemory,
}

impl Segment {
    /// Number of physical segments.
    pub const COUNT: usize = 4;
}

impl From<Segment> for usize {
    fn from(segment: Segment) -> Self {
        match segment {
            Segment::Dma => 0,
            Segment::DirectMap => 1,
            Segment::Dma32 => 2,
            Segment::HighMemory => 3,
        }
    }
}
