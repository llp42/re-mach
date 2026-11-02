// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Host information.

/// The EFI system table address `host_info` reports when UEFI is not
/// available.
#[expect(dead_code, reason = "host_info reports it")]
pub const HOST_NO_UEFI: u64 = u64::MAX;
