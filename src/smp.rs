// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data.

use core::sync::atomic::AtomicPtr;

use crate::arch;

/// One CPU's state.
#[repr(C)]
pub struct PerCpu {
    /// Points to itself; first, so the arch can read it at offset 0.
    pub this: AtomicPtr<PerCpu>,
    /// This CPU's index.
    pub id: usize,
    /// Machine-dependent part.
    pub md: arch::PerCpu,
}

/// CPU 0's state: static, since there is no allocator in early boot.
pub static PER_CPU_0: PerCpu = PerCpu {
    this: AtomicPtr::new((&raw const PER_CPU_0).cast_mut()),
    id: 0,
    md: arch::PerCpu::new(),
};
