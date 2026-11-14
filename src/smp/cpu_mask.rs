// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Sets of CPUs.

use core::iter;
use core::sync::atomic::{AtomicU64, Ordering};

/// An atomic set of CPU numbers, a bit each, for up to 256 CPUs.
pub struct CpuMask([AtomicU64; 4]);

impl CpuMask {
    /// No CPU.
    pub const fn empty() -> Self {
        Self([const { AtomicU64::new(0) }; 4])
    }

    /// Every CPU.
    pub const fn all() -> Self {
        Self([const { AtomicU64::new(u64::MAX) }; 4])
    }

    /// Adds `cpu`, with a `SeqCst` locked read-modify-write.
    pub fn insert(&self, cpu: usize) {
        self.0[cpu / 64].fetch_or(1 << (cpu % 64), Ordering::SeqCst);
    }

    /// Removes `cpu`, with a `SeqCst` locked read-modify-write.
    pub fn remove(&self, cpu: usize) {
        self.0[cpu / 64].fetch_and(!(1 << (cpu % 64)), Ordering::SeqCst);
    }

    /// Whether it holds `cpu`, loaded `Relaxed`: only the current CPU's own bit
    /// may be read this way, as only that CPU changes it.
    pub fn contains(&self, cpu: usize) -> bool {
        self.0[cpu / 64].load(Ordering::Relaxed) & 1 << (cpu % 64) != 0
    }

    /// Whether it holds a CPU other than `cpu`, each word loaded `SeqCst`.
    pub fn contains_other(&self, cpu: usize) -> bool {
        self.0.iter().enumerate().any(|(word, bits)| {
            let mut others = bits.load(Ordering::SeqCst);
            if word == cpu / 64 {
                others &= !(1 << (cpu % 64));
            }
            others != 0
        })
    }

    /// The CPUs it holds, in order, each word loaded `Acquire`.
    pub fn iter(&self) -> impl Iterator<Item = usize> + '_ {
        self.0.iter().enumerate().flat_map(|(word, bits)| {
            let mut bits = bits.load(Ordering::Acquire);
            iter::from_fn(move || {
                (bits != 0).then(|| {
                    let bit = bits.trailing_zeros();
                    bits &= bits - 1;
                    word * 64 + usize::try_from(bit).unwrap()
                })
            })
        })
    }
}
