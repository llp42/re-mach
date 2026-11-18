// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The device calls' server routines, as MIG makes them from `device`'s
//! definitions; only those written so far.

use core::ptr;
use core::sync::atomic::Ordering;

use super::mig::{self, DeviceName};
use crate::device;
use crate::error::Error;
use crate::ipc::port::Port;
use crate::reference::Reference;

mig::subsystem! {
    base 2800;
    0 device_open(master: Port, mode: u32, name: DeviceName) -> (device: Port);
}

/// Serves `device_open`: the device `name` names, opened through `master`;
/// gives a send right for its port. The mode goes unused: `time` ignores
/// it.
///
/// # Panics
///
/// If `master` is not the master device port: not yet. As `device::open`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn device_open(master: &Port, _mode: u32, name: DeviceName) -> Result<Reference<Port>, Error> {
    assert!(
        ptr::eq(master, device::MASTER_PORT.load(Ordering::Acquire)),
        "ds_device_open: D_INVALID_OPERATION, not yet"
    );
    Ok(device::open(name.as_bytes()))
}
