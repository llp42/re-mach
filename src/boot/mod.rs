// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Kernel handoff data and boot-environment adapters.

pub mod info;
#[cfg(target_os = "uefi")]
pub mod uefi;
