// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The device calls' server routines, as MIG makes them from `device`'s
//! definitions; only those written so far.

use core::ptr;
use core::sync::atomic::Ordering;

use super::mig;
use super::mig::device_name::DeviceName;
use crate::device::{self, Device};
use crate::error::Error;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::vm::object::offset::Offset;

mig::subsystem! {
    base 2800;
    0 device_open(master: Port, mode: u32, name: DeviceName) -> (device: Port);
    1 device_close(device = Port::to_device);
    9 device_map(
        device = Port::to_device,
        protection: u32,
        offset: Offset,
        size: usize,
        unmap: i32
    ) -> (pager: Port);
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

/// Serves `device_close`: `device` closed, once its last open does.
///
/// # Errors
///
/// `NoSuchDevice` for no device.
fn device_close(device: Option<&Reference<Device>>) -> Result<(), Error> {
    let Some(device) = device else {
        return Err(Error::NoSuchDevice);
    };
    device::close(device);
    Ok(())
}

/// Serves `device_map`: `device`'s memory from `offset`, allowing
/// `protection`; gives a send right for its pager's port. `size` and
/// `unmap` go unused: GNU Mach never reads them.
///
/// # Panics
///
/// If `device` is dead or no device: not yet. As `device::map`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn device_map(
    device: Option<&Reference<Device>>,
    protection: u32,
    offset: Offset,
    _size: usize,
    _unmap: i32,
) -> Result<Reference<Port>, Error> {
    let device = device.unwrap_or_else(|| panic!("ds_device_map: D_NO_SUCH_DEVICE, not yet"));
    Ok(device::map(device, protection, offset))
}
