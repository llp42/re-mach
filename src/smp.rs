// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data.

use core::ptr;
use core::sync::atomic::AtomicPtr;

use crate::arch;
use crate::kern::Thread;

/// One CPU's state.
#[repr(C)]
pub struct PerCpu {
    /// Points to itself; first, so the arch can read it at offset 0.
    pub this: AtomicPtr<Self>,
    /// This CPU's index.
    pub id: usize,
    /// The running thread; never null once set (`gd_curthread`).
    pub current_thread: AtomicPtr<Thread>,
    /// Machine-dependent part.
    pub md: arch::PerCpu,
}

/// CPU 0's state: static, since there is no allocator in early boot.
pub static PER_CPU_0: PerCpu = PerCpu {
    this: AtomicPtr::new((&raw const PER_CPU_0).cast_mut()),
    id: 0,
    current_thread: AtomicPtr::new(ptr::null_mut()),
    md: arch::PerCpu::new(),
};
