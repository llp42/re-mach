// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Boot information, read in place from the boot loader.

pub mod multiboot2;

use core::ffi::CStr;
use core::ops::Range;

use crate::arch::PhysAddr;

/// A boot module, copied into boot memory.
pub struct BootModule {
    /// Its physical range.
    pub range: Range<PhysAddr>,
    pub command_line: &'static CStr,
}

/// Prints the kernel's name and version.
pub fn banner() {
    crate::println!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
}
