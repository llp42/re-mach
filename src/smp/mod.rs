// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Multiprocessor support.

mod cpu_mask;
mod per_cpu;

pub use cpu_mask::CpuMask;
pub use per_cpu::{MachinePerCpu, PerCpu, current, of};

/// The CPUs using the kernel's page tables: kernel TLB shootdowns reach
/// them.
pub static STARTED: CpuMask = CpuMask::empty();
