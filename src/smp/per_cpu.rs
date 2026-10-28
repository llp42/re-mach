// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data, implemented by the arch.

/// The current CPU's data.
pub trait PerCpu {
    /// The current CPU's number.
    fn cpu_id() -> usize;
}
