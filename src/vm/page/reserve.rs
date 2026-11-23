// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use super::page::Page;
use super::pool::pages;
use crate::arch::PAGE_SIZE;
use crate::sync::Spinlock;

/// Most low memory kept for DMA.
const DMA_RESERVE_LIMIT: usize = 256 << 20;

/// The frame the DMA reserve ends before, for a span of `size` bytes.
pub(super) fn reserve_end(size: usize) -> usize {
    DMA_RESERVE_LIMIT.min(size / 4) / PAGE_SIZE
}

/// The DMA reserve's free frames and their count.
pub(super) struct Reserve {
    /// A bit per frame from 0, set when it is free.
    frames: [u64; DMA_RESERVE_LIMIT / PAGE_SIZE / 64],
    /// How many bits are set.
    pub(super) free: usize,
}

impl Reserve {
    /// Takes its highest free frame, keeping the lowest for DMA longest.
    fn take(&mut self) -> Option<usize> {
        let word = self.frames.iter().rposition(|&bits| bits != 0)?;
        let bit = usize::try_from(self.frames[word].ilog2()).unwrap();
        self.frames[word] &= !(1 << bit);
        self.free -= 1;
        Some(word * 64 + bit)
    }

    /// Puts `frame` in, if it is not in already.
    pub(super) const fn put(&mut self, frame: usize) {
        let bit = 1 << (frame % 64);
        if self.frames[frame / 64] & bit == 0 {
            self.frames[frame / 64] |= bit;
            self.free += 1;
        }
    }

    /// Whether `frame` is free.
    const fn get(&self, frame: usize) -> bool {
        self.frames[frame / 64] & (1 << (frame % 64)) != 0
    }

    /// Frees `frame`.
    const fn clear(&mut self, frame: usize) {
        self.frames[frame / 64] &= !(1 << (frame % 64));
    }

    /// Takes `count` contiguous free frames from `first` up to `last`, as
    /// `vm_page_grab_contig` does for the DMA segment; gives the first.
    pub(super) fn take_contiguous(
        &mut self,
        count: usize,
        first: usize,
        last: usize,
    ) -> Option<usize> {
        let last = last.min(self.frames.len() * 64);
        let mut run = 0;
        let mut start = first;
        for frame in first..last {
            if !self.get(frame) {
                run = 0;
                continue;
            }
            if run == 0 {
                start = frame;
            }
            run += 1;
            if run == count {
                for frame in start..start + count {
                    self.clear(frame);
                }
                self.free -= count;
                return Some(start);
            }
        }
        None
    }
}

/// Free frames kept for DMA, below `DMA_RESERVE_LIMIT`; allocations take
/// them once the colour queues are empty.
pub(super) static DMA_RESERVE: Spinlock<Reserve> = Spinlock::new(Reserve {
    frames: [0; DMA_RESERVE_LIMIT / PAGE_SIZE / 64],
    free: 0,
});

/// Takes the page of the DMA reserve's highest free frame, as GNU Mach's
/// segment walk ends at its DMA segment; `None` when the reserve is empty.
pub(super) fn allocate_reserved() -> Option<&'static Page> {
    let array = pages();
    let mut reserve = DMA_RESERVE.lock();
    let frame = reserve.take();
    drop(reserve);
    Some(&array.pages[frame? - array.start.0 / PAGE_SIZE])
}
