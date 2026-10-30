// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Host information.

use core::sync::atomic::AtomicU64;

/// EFI system table address when UEFI is not available.
pub const HOST_NO_UEFI: u64 = u64::MAX;

/// Physical address of the EFI system table, or `HOST_NO_UEFI`.
pub static EFI_SYSTEM_TABLE_ADDRESS: AtomicU64 = AtomicU64::new(HOST_NO_UEFI);
