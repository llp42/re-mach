// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Devices; so far only the master device port, device lookup, opening
//! and mapping, the device pager, terminals' input settings, the
//! interrupt thread and the `time` device.

pub mod interrupt;
pub mod lookup;
pub mod pager;
mod terminal;
mod time;

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink};

use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::reference::{Count, Counted, Reference};
use crate::sync::Spinlock;
use crate::vm::Protection;
use crate::vm::object::offset::Offset;

/// The master device port, which devices are opened through; once
/// `create_service` makes it.
pub static MASTER_PORT: AtomicPtr<Port> = AtomicPtr::new(ptr::null_mut());

/// Creates the device service, in the kernel task: so far only the master
/// device port, a port the kernel receives on that stands for nothing, then
/// device lookup, the device pager and terminals' input settings.
///
/// # Panics
///
/// If the port can't be allocated.
pub fn create_service() {
    let port = Port::allocate_kernel()
        .unwrap_or_else(|error| panic!("can't allocate master device port: {error:?}"));
    // The slot keeps its reference.
    MASTER_PORT.store(
        Reference::into_raw(Reference::from(port)).as_ptr(),
        Ordering::Release,
    );
    lookup::init();
    pager::init();
    terminal::init();
}

/// A device: its kind and unit, its references, its place in the lookup
/// list, and its state under its lock.
pub struct Device {
    /// How many references it has.
    references: Count,
    /// Its kind.
    kind: Kind,
    /// Its unit.
    unit: u32,
    /// Its place in the lookup list.
    link: LinkedListAtomicLink,
    /// Its state.
    state: Spinlock<DeviceState>,
}

/// A device's state under its lock; so far only how far it is open, its
/// open count, its port and its pagers.
struct DeviceState {
    /// How far it is open.
    state: State,
    /// How many opens it has.
    open_count: u32,
    /// Its port, once opening.
    port: Option<Reference<Port>>,
    /// Its pagers, one per offset.
    pagers: LinkedList<pager::PagerAdapter>,
}

/// How far a device is open; only the states used here.
#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    /// Not open.
    Init,
    /// Being opened.
    Opening,
    /// Open.
    Open,
    /// Being closed.
    Closing,
}

/// A kind of device the kernel serves; only `time` so far.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// `time`, the mapped time.
    Time,
}

impl Kind {
    /// Every kind, in the name table's order.
    const ALL: [Self; 1] = [Self::Time];

    /// Its name, which its devices are opened by.
    const fn name(self) -> &'static [u8] {
        match self {
            Self::Time => b"time",
        }
    }
}

/// Opens the device `name` names, as `device_open` does: found or made,
/// given a port, opened by its kind, then open once; gives a send right
/// for its port. `time`'s open neither fails nor queues.
///
/// # Panics
///
/// If the device is being opened or is open, or no port can be allocated:
/// not yet. As `lookup::find`.
pub fn open(name: &[u8]) -> Reference<Port> {
    let device = lookup::find(name);
    let mut state = device.state.lock();
    assert!(
        state.state != State::Opening,
        "device_open: thread_sleep on an opening device, not yet"
    );
    assert!(
        state.state != State::Closing,
        "device_open: thread_sleep on a closing device, not yet"
    );
    assert!(
        state.state != State::Open,
        "device_open: an open device, not yet"
    );
    state.state = State::Opening;
    drop(state);
    let port = Reference::from(
        Port::allocate_kernel()
            .unwrap_or_else(|_| panic!("device_open: KERN_RESOURCE_SHORTAGE, not yet")),
    );
    // dev_port_enter: the port takes a reference to the device.
    port.set_kernel_object(KernelObject::Device(Reference::into_raw(device.clone())));
    device.state.lock().port = Some(port);
    // The kind's open: `time`'s does nothing (timeopen).
    match device.kind {
        Kind::Time => {}
    }
    // ds_open_done's success, then mach_convert_device_to_port's send right.
    let mut state = device.state.lock();
    state.state = State::Open;
    state.open_count = 1;
    let send = Port::make_send(state.port.as_ref().unwrap());
    drop(state);
    // The lookup's reference goes, as mach_device_deallocate.
    drop(device);
    send
}

/// Closes `device`, as `device_close` does: its last open takes its port
/// away and its kind's close runs, then it can be opened again.
///
/// # Panics
///
/// If it is not open, or its port is not there: not yet. As
/// `Port::deallocate_kernel`.
pub fn close(device: &Reference<Device>) {
    let port = {
        let mut state = device.state.lock();
        if state.open_count > 1 {
            state.open_count -= 1;
            return;
        }
        assert!(
            state.state == State::Open,
            "ds_device_close: D_NO_SUCH_DEVICE, not yet"
        );
        // Mark it closing, to prevent new IO.
        state.state = State::Closing;
        state.open_count = 0;
        state.port.take()
    };
    let Some(port) = port else {
        panic!("device_close: a device with no port");
    };
    // As dev_port_remove: the port no longer stands for the device, and
    // its reference to it goes.
    if let Some(KernelObject::Device(pointer)) = port.replace_kernel_object(None) {
        // SAFETY: the port's kobject held a reference to the device, given
        // up by `into_raw`, which this takes.
        drop(unsafe { Reference::from_raw(pointer) });
    }
    port.deallocate_kernel();
    drop(port);
    // The kind's close: `time`'s does nothing.
    match device.kind {
        Kind::Time => {}
    }
    // Closed: someone opening it may proceed.
    device.state.lock().state = State::Init;
}

/// Maps `device`'s memory from `offset`, allowing `protection`, as
/// `device_map` does; gives a send right for its pager's port.
///
/// # Panics
///
/// If `protection` holds more than reading, writing and running, or the
/// device is not open: not yet. As `pager::setup`.
pub fn map(device: &Reference<Device>, protection: u32, offset: Offset) -> Reference<Port> {
    let protection = Protection::from_bits(protection)
        .unwrap_or_else(|| panic!("device_map: KERN_INVALID_ARGUMENT, not yet"));
    assert!(
        device.state.lock().state == State::Open,
        "device_map: D_NO_SUCH_DEVICE, not yet"
    );
    pager::setup(device, protection, offset)
}

// SAFETY: its count is its own, and every device is a box of its lookup's
// cache, which `free` takes back.
unsafe impl Counted for Device {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `device`, as `mach_device_deallocate` does at zero: it leaves
    /// the lookup list, then goes back to its cache.
    unsafe fn free(device: NonNull<Self>) {
        // SAFETY: the caller's contract.
        unsafe { lookup::free(device) };
    }
}
