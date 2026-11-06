// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! re-mach: a reimplementation of the Mach microkernel in Rust, for `x86_64`.

#![no_std]
#![no_main]

mod arch;
mod boot;
mod collections;
mod config;
mod device;
mod ipc;
mod kernel;
mod smp;
mod sync;
mod vm;
