// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Host information.

use core::ptr;
use core::sync::atomic::AtomicPtr;

use crate::ipc::port::Port;

/// The host: the machine as a whole; so far only its ports.
pub struct Host {
    /// Its port, for information about it.
    pub(super) port: AtomicPtr<Port>,
    /// Its privileged port, for controlling it.
    pub(super) privileged_port: AtomicPtr<Port>,
}

/// The host, whose ports `ipc_host::init` makes.
pub static HOST: Host = Host {
    port: AtomicPtr::new(ptr::null_mut()),
    privileged_port: AtomicPtr::new(ptr::null_mut()),
};

/// The EFI system table address `host_info` reports when UEFI is not
/// available.
#[expect(dead_code, reason = "host_info reports it")]
pub const HOST_NO_UEFI: u64 = u64::MAX;
