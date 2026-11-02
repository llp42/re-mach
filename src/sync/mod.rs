// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Cells statics can hold.

mod boot_cell;
mod once_lock;

pub use boot_cell::BootCell;
pub use once_lock::OnceLock;
