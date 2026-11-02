// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Application processor startup.

use super::address::PhysAddr;
use crate::sync::OnceLock;

/// Physical address of the AP boot page, once boot memory sets it aside.
pub static ADDRESS: OnceLock<PhysAddr> = OnceLock::new();
