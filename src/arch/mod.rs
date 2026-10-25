// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Compile-time selection of the CPU, boot entry, early console and user
//! program constants.

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{ELF_MACHINE, PAGE_SIZE, cpu::halt, entry, serial};

#[cfg(not(target_arch = "x86_64"))]
compile_error!("no architecture implementation for this target");
