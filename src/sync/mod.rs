// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Cells statics can hold.

mod boot_cell;
mod critical_section;
mod once_lock;
mod spinlock;

pub use boot_cell::BootCell;
pub use critical_section::CriticalSection;
pub use once_lock::OnceLock;
pub use spinlock::Spinlock;
