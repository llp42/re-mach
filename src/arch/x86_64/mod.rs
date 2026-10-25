// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! x86_64 implementation.

pub mod cpu;
pub mod entry;
pub mod serial;

/// ELF machine of the kernel and of user executables.
pub const ELF_MACHINE: u16 = 62;
/// Size of a base page.
pub const PAGE_SIZE: u64 = 4096;
