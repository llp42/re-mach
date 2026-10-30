// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Machine-dependent virtual memory parameters.

use core::ptr;

use super::address::PhysAddr;

/// Bytes per page.
pub const PAGE_SIZE: u64 = 4096;

/// Start of the kernel's virtual addresses.
pub const MIN_KERNEL_ADDRESS: u64 = 0xffff_ffff_8000_0000;

/// Last of the kernel's virtual addresses.
pub const MAX_KERNEL_ADDRESS: u64 = 0xffff_ffff_ffff_ffff;

/// Kernel map size; the rest of the kernel window maps physical memory directly.
pub const KERNEL_MAP_SIZE: u64 = 1024 * 1024 * 1024;

/// Kernel virtual address of the physical `address`, physical memory being
/// mapped directly from `MIN_KERNEL_ADDRESS`.
#[expect(dead_code, reason = "the command line copy calls it")]
pub fn phys_to_virt<T>(address: PhysAddr) -> *mut T {
    let virt = address.0 + MIN_KERNEL_ADDRESS;
    ptr::with_exposed_provenance_mut(usize::try_from(virt).unwrap())
}

/// End of the memory ISA DMA reaches.
pub const DMA_LIMIT: PhysAddr = PhysAddr(0x100_0000);

/// End of the memory 32-bit DMA reaches.
pub const DMA32_LIMIT: PhysAddr = PhysAddr(0x1_0000_0000);

/// End of the physical memory mapped directly in the kernel's space.
pub const DIRECT_MAP_LIMIT: PhysAddr =
    PhysAddr(MAX_KERNEL_ADDRESS - MIN_KERNEL_ADDRESS - KERNEL_MAP_SIZE + 1);

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
