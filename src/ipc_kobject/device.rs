// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The device calls' server routines, as MIG makes them from `device`'s
//! definitions; only those written so far.

use core::ptr;
use core::sync::atomic::Ordering;

use super::mig;
use super::mig::argument::{Char, OutOfLine};
use super::mig::device_name::DeviceName;
use crate::arch::PAGE_SIZE;
use crate::device::{self, Device};
use crate::error::Error;
use crate::ipc::KERNEL_MAP;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::vm;
use crate::vm::Protection;
use crate::vm::map::change::{Placement, RangeCheck, Source};
use crate::vm::object::offset::Offset;

mig::subsystem! {
    base 2800;
    0 device_open(master: Port, mode: u32, name: DeviceName) -> (device: Port);
    1 device_close(device = Port::to_device);
    2 device_write(device = Port::to_device, mode: u32, recnum: usize, data: OutOfLine<u8>)
        -> (bytes_written: i32);
    3 device_write_inband(
        device = Port::to_device,
        mode: u32,
        recnum: usize,
        data: [Char; ..128]
    ) -> (bytes_written: i32);
    4 device_read(device = Port::to_device, mode: u32, recnum: usize, bytes_wanted: i32)
        -> (data: OutOfLine<u8>);
    5 device_read_inband(
        device = Port::to_device,
        mode: u32,
        recnum: usize,
        bytes_wanted: i32
    ) -> (data: [Char; ..128], CountOut);
    9 device_map(
        device = Port::to_device,
        protection: u32,
        offset: Offset,
        size: usize,
        unmap: i32
    ) -> (pager: Port);
    10 device_set_status(device = Port::to_device, flavor: u32, status: [i32; ..1024]);
    11 device_get_status(device = Port::to_device, flavor: u32) -> (status: [i32; ..1024], CountInOut);
    12 device_set_filter(
        device = Port::to_device,
        receive_port: Port,
        priority: i32,
        filter: [u16; ..128]
    );
    13 device_intr_register(device = Port::to_device, id: i32, flags: i32, receive_port: Port);
    14 device_intr_ack(device = Port::to_device, receive_port: Port);
}

/// Serves `device_open`: the device `name` names, opened through `master`;
/// gives a send right for its port. The mode goes unused: `time` ignores
/// it.
///
/// # Errors
///
/// `InvalidOperation` if `master` is not the master device port, and
/// `NoSuchDevice` if no device has that name.
///
/// # Panics
///
/// As `device::open`.
fn device_open(master: &Port, _mode: u32, name: DeviceName) -> Result<Reference<Port>, Error> {
    if !ptr::eq(master, device::MASTER_PORT.load(Ordering::Acquire)) {
        return Err(Error::InvalidOperation);
    }
    device::open(name.as_bytes())
}

/// Serves `device_close`: `device` closed, once its last open does.
///
/// # Errors
///
/// `NoSuchDevice` for no device.
///
/// # Panics
///
/// As `device::close`.
fn device_close(device: Option<&Reference<Device>>) -> Result<(), Error> {
    let Some(device) = device else {
        return Err(Error::NoSuchDevice);
    };
    device::close(device);
    Ok(())
}

/// Serves `device_write`: `data` written to `device`; tells `reply_to`, its
/// request's reply port, how much of it was.
///
/// # Errors
///
/// `InvalidOperation` if its kind does not write.
///
/// # Panics
///
/// If it is not open, or no memory can be got: not yet. As
/// `device::write`.
fn device_write(
    device: Option<&Reference<Device>>,
    _mode: u32,
    _recnum: usize,
    data: Option<OutOfLine<u8>>,
) -> Result<i32, Error> {
    let Some(device) = device else {
        return Err(Error::NoSuchDevice);
    };
    let Some(data) = data else {
        return Ok(0);
    };
    let count = data.count();
    let buffer = crate::kalloc::allocate(count)
        .unwrap_or_else(|error| panic!("device_write: kalloc: {error:?}, not yet"));
    // SAFETY: `count` bytes, ours alone, untouched before the read fills
    // them.
    let bytes = unsafe { core::slice::from_raw_parts_mut(buffer.as_ptr(), count) };
    data.into_copy().read_pages(0, bytes);
    let written = device::write(device, bytes);
    // SAFETY: from `kalloc::allocate`, `count` bytes, not used again.
    unsafe { crate::kalloc::free(buffer, count) };
    Ok(i32::try_from(written?).unwrap())
}

/// Serves `device_write_inband`: `data` written to `device`; tells
/// `reply_to`, its request's reply port, how much of it was.
///
/// # Errors
///
/// `InvalidOperation` if its kind does not write.
///
/// # Panics
///
/// If it is not open: not yet. As `device::write`.
fn device_write_inband(
    device: Option<&Reference<Device>>,
    _mode: u32,
    _recnum: usize,
    data: &[Char],
) -> Result<i32, Error> {
    let Some(device) = device else {
        return Err(Error::NoSuchDevice);
    };
    let written = device::write(device, Char::bytes(data))?;
    Ok(i32::try_from(written).unwrap())
}

/// Serves `device_read`: up to `bytes_wanted` bytes of `device`, out of
/// line; tells `reply_to`, its request's reply port, which they are.
///
/// # Errors
///
/// `InvalidOperation` if its kind does not read.
///
/// # Panics
///
/// If it is not open, or no memory can be got: not yet. As `device::read`.
fn device_read(
    device: Option<&Reference<Device>>,
    _mode: u32,
    _recnum: usize,
    bytes_wanted: i32,
) -> Result<OutOfLine<u8>, Error> {
    let Some(device) = device else {
        return Err(Error::NoSuchDevice);
    };
    let wanted = usize::try_from(bytes_wanted).unwrap_or(0);
    let data = device::read(device, wanted)?;
    Ok(out_of_line(data))
}

/// Serves `device_read_inband`: up to `bytes_wanted` bytes of `device`, in
/// line; tells `reply_to`, its request's reply port, which they are.
///
/// # Errors
///
/// `InvalidOperation` if its kind does not read.
///
/// # Panics
///
/// If it is not open: not yet. As `device::read`.
fn device_read_inband(
    device: Option<&Reference<Device>>,
    _mode: u32,
    _recnum: usize,
    bytes_wanted: i32,
) -> Result<(usize, [Char; 128]), Error> {
    let Some(device) = device else {
        return Err(Error::NoSuchDevice);
    };
    let wanted = usize::try_from(bytes_wanted).unwrap_or(0);
    let data = device::read(device, wanted)?;
    let mut buffer = [Char::new(0); 128];
    for (slot, byte) in buffer.iter_mut().zip(data.iter()) {
        *slot = Char::new(*byte);
    }
    Ok((data.len(), buffer))
}

/// Serves `device_set_status`: nothing the kernel serves has statuses.
///
/// # Errors
///
/// `InvalidOperation`, as `device::status`.
const fn device_set_status(
    device: Option<&Reference<Device>>,
    _flavor: u32,
    _status: &[i32],
) -> Result<(), Error> {
    let Some(_device) = device else {
        return Err(Error::NoSuchDevice);
    };
    device::status()
}

/// Serves `device_get_status`: nothing the kernel serves has statuses.
///
/// # Errors
///
/// `InvalidOperation`, as `device::status`.
fn device_get_status(
    device: Option<&Reference<Device>>,
    _flavor: u32,
    _status: &mut [i32],
) -> Result<usize, Error> {
    let Some(_device) = device else {
        return Err(Error::NoSuchDevice);
    };
    device::status()?;
    unreachable!("device::status never succeeds")
}

/// Serves `device_set_filter`: nothing the kernel serves filters.
///
/// # Errors
///
/// `InvalidOperation`, always.
const fn device_set_filter(
    device: Option<&Reference<Device>>,
    _receive_port: Option<&Reference<Port>>,
    _priority: i32,
    _filter: &[u16],
) -> Result<(), Error> {
    let Some(_device) = device else {
        return Err(Error::NoSuchDevice);
    };
    Err(Error::InvalidOperation)
}

/// Serves `device_intr_register`: `device`'s interrupt `id` registered for
/// `receive_port`.
///
/// # Errors
///
/// As `device::intr_register`.
///
/// # Panics
///
/// As `device::intr_register`.
fn device_intr_register(
    device: Option<&Reference<Device>>,
    id: i32,
    flags: i32,
    _receive_port: Option<&Reference<Port>>,
) -> Result<(), Error> {
    let Some(device) = device else {
        return Err(Error::NoSuchDevice);
    };
    device::intr_register(device, id, flags)
}

/// Serves `device_intr_ack`: `device`'s interrupt for `receive_port`
/// acknowledged.
///
/// # Errors
///
/// As `device::intr_ack`.
fn device_intr_ack(
    device: Option<&Reference<Device>>,
    _receive_port: Option<&Reference<Port>>,
) -> Result<(), Error> {
    let Some(device) = device else {
        return Err(Error::NoSuchDevice);
    };
    device::intr_ack(device)
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

/// `bytes` out of line, in a copy of the IPC kernel map, as MIG carries
/// device reads; empty for none.
///
/// # Panics
///
/// If the IPC map can't be allocated from or made pageable: not yet. As
/// `vm::user::allocate` and `Map::pageable`.
fn out_of_line(bytes: &[u8]) -> OutOfLine<u8> {
    if bytes.is_empty() {
        return OutOfLine::empty();
    }
    let map = KERNEL_MAP.get().expect("device_read: no ipc_kernel_map");
    let size = bytes.len().next_multiple_of(PAGE_SIZE);
    let start = vm::user::allocate(map, Placement::Anywhere, size)
        .unwrap_or_else(|error| panic!("device_read: vm_allocate: {error:?}, not yet"));
    let range = start..start + size;
    if let Err(error) = map.pageable(
        range.clone(),
        Protection::READ | Protection::WRITE,
        RangeCheck::Checked,
    ) {
        panic!("device_read: vm_map_pageable: {error:?}");
    }
    // SAFETY: the allocation is `size` bytes of the IPC map, page-aligned
    // and wired here; nothing else reads or writes it.
    unsafe {
        let destination = ptr::with_exposed_provenance_mut::<u8>(usize::from(start));
        ptr::copy_nonoverlapping(bytes.as_ptr(), destination, bytes.len());
        destination
            .add(bytes.len())
            .write_bytes(0, size - bytes.len());
    }
    if let Err(error) = map.pageable(range.clone(), Protection::empty(), RangeCheck::Checked) {
        panic!("device_read: vm_map_pageable: {error:?}");
    }
    OutOfLine::new(map.copy_in(range, Source::Destroy), bytes.len())
}
