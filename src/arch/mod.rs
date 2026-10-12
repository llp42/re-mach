// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Compile-time selection of the CPU and early console implementation.

#[cfg(target_arch = "x86_64")]
mod x86_64;

#[cfg(target_arch = "x86_64")]
pub(crate) use x86_64::{cpu::halt, serial};

#[cfg(not(target_arch = "x86_64"))]
compile_error!("no architecture implementation for this target");
