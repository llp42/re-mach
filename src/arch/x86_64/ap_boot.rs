// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Application processor startup.

use core::sync::atomic::AtomicUsize;

/// Physical address of the AP boot page.
pub static ADDRESS: AtomicUsize = AtomicUsize::new(0);
