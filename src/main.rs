// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! re-mach: a reimplementation of the Mach microkernel in Rust, for `x86_64`.

#![no_std]
#![no_main]

mod arch;
mod ast;
mod boot;
mod bootstrap;
mod clock;
mod collections;
mod config;
mod console;
mod device;
mod error;
mod gsync;
mod host;
mod ipc;
mod ipc_host;
mod kalloc;
mod machine;
mod panic;
mod processor;
mod sched;
mod slab;
mod smp;
mod startup;
mod sync;
mod syscall;
mod task;
mod thread;
mod vm;
