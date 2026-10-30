// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Boot information, read in place from the boot loader.

pub mod multiboot2;

use core::fmt::Write;

/// Prints the kernel's name and version.
pub fn banner(out: &mut impl Write) {
    let _ = write!(
        out,
        "{} {}\r\n",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );
}
