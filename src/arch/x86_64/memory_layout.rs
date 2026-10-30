// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Machine-dependent virtual memory parameters.

use super::address::PhysAddr;

/// Bytes per page.
pub const PAGE_SIZE: u64 = 4096;

/// End of the memory ISA DMA reaches.
pub const DMA_LIMIT: PhysAddr = PhysAddr(0x100_0000);

/// Physical segments.
#[derive(Clone, Copy)]
pub enum Segment {
    Dma,
}

impl Segment {
    /// Number of physical segments.
    pub const COUNT: usize = 4;
}

impl From<Segment> for usize {
    fn from(segment: Segment) -> Self {
        match segment {
            Segment::Dma => 0,
        }
    }
}
