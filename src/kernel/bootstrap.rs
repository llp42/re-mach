// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The bootstrap: tasks for the modules the loader brought.

use crate::arch::{self, Machine as _};

/// Starts the boot modules' tasks; so far it only finds the modules.
///
/// # Panics
///
/// If the loader brought no module, or a single one with nothing past its
/// command line's first word: compat mode, not yet.
pub fn create() {
    let modules = arch::Current::boot_modules();
    assert!(
        !modules.is_empty(),
        "No bootstrap code loaded with the kernel!"
    );
    if let [module] = modules {
        let line = module.command_line.to_bytes();
        // Nothing but spaces and newlines past the first space, if any.
        let rest = line.splitn(2, |&byte| byte == b' ').nth(1);
        let compat = rest
            .unwrap_or_default()
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\n'));
        assert!(
            !compat,
            "bootstrap_create: a single module in compat mode, not yet"
        );
    }
}
