// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Mach's calls' server routines, as MIG makes them from `mach`'s
//! definitions; only those written so far.

use core::ptr::{self, NonNull};

use super::mig::{self, Boolean, OutOfLine};
use crate::arch;
use crate::error::Error;
use crate::ipc::port::Port;
use crate::reference::Reference;
use crate::smp;
use crate::task::{self, Task};
use crate::thread::{self, MachineThread as _, Thread};
use crate::vm::map::{Change, Map, Placement};
use crate::vm::object::Offset;
use crate::vm::user::Mapping;
use crate::vm::{self, Inheritance, Protection, VirtualAddress};

mig::subsystem! {
    base 2000;
    17 thread_get_state(target_thread = thread::of, flavor: i32)
        -> (old_state: [u32; ..1024], CountInOut);
    21 vm_allocate(
        target_task = task::map_of,
        address: VirtualAddress,
        size: usize,
        anywhere: Boolean
    ) -> (address: VirtualAddress);
    23 vm_deallocate(target_task = task::map_of, address: VirtualAddress, size: usize);
    24 vm_protect(
        target_task = task::map_of,
        address: VirtualAddress,
        size: usize,
        set_maximum: Boolean,
        new_protection: u32
    );
    27 vm_write(target_task = task::map_of, address: VirtualAddress, data: OutOfLine<u8>);
    61 thread_create(parent_task = task::of) -> (child_thread: Port);
    89 vm_map(
        target_task = task::map_of,
        address: VirtualAddress,
        size: usize,
        mask: usize,
        anywhere: Boolean,
        memory_object: Port,
        offset: Offset,
        copy: Boolean,
        cur_protection: u32,
        max_protection: u32,
        inheritance: u32
    ) -> (address: VirtualAddress);
}

/// Serves `thread_get_state`: `thread`'s machine state of `flavor`, in at
/// most `state`'s words; gives how many.
///
/// # Errors
///
/// As `Thread::read_state`.
///
/// # Panics
///
/// As `Thread::read_state`.
fn thread_get_state(
    thread: Option<&Reference<Thread>>,
    flavor: i32,
    state: &mut [u32],
) -> Result<usize, Error> {
    // SAFETY: the handle keeps the thread alive through the call; threads
    // are never freed yet.
    let thread = thread.map(|thread| unsafe { Reference::as_ptr(thread).as_ref() });
    Thread::read_state(thread, flavor, state)
}

/// Serves `vm_allocate`: `size` bytes of zero-filled memory mapped into
/// `map` at `address` or anywhere; gives where.
///
/// # Errors
///
/// `InvalidArgument` for no map; as `vm::user::allocate`.
///
/// # Panics
///
/// As `vm::user::allocate`.
fn vm_allocate(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    size: usize,
    anywhere: Boolean,
) -> Result<VirtualAddress, Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    let placement = if bool::from(anywhere) {
        Placement::Anywhere
    } else {
        Placement::Fixed(address)
    };
    vm::user::allocate(map, placement, size)
}

/// Serves `vm_deallocate`: `size` bytes of `map` from `address` removed.
///
/// # Errors
///
/// `InvalidArgument` for no map.
///
/// # Panics
///
/// As `vm::user::deallocate`.
fn vm_deallocate(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    size: usize,
) -> Result<(), Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    vm::user::deallocate(map, address, size);
    Ok(())
}

/// Serves `vm_protect`: what `size` bytes of `map` from `address` allow,
/// or at most may allow if `set_maximum`, set to `protection`.
///
/// # Errors
///
/// `InvalidArgument` for no map; as `vm::user::protect`.
///
/// # Panics
///
/// As `vm::user::protect`.
fn vm_protect(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    size: usize,
    set_maximum: Boolean,
    protection: u32,
) -> Result<(), Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    let change = if bool::from(set_maximum) {
        Change::Maximum
    } else {
        Change::Current
    };
    vm::user::protect(
        map,
        address,
        size,
        change,
        Protection::from_bits_retain(protection),
    )
}

/// Serves `vm_write`: the memory `data` holds written over `map` from
/// `address`.
///
/// # Errors
///
/// `InvalidArgument` for no map; as `vm::user::write`.
///
/// # Panics
///
/// For no map, as discarding a copy holding entries: not yet. As
/// `vm::user::write`.
fn vm_write(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    data: Option<OutOfLine<u8>>,
) -> Result<(), Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    vm::user::write(map, address, data.map(OutOfLine::into_copy))
}

/// Serves `thread_create`: a new thread in `task`, suspended, which starts
/// by returning to user mode; gives a send right for its port.
///
/// # Errors
///
/// `InvalidArgument` for no task; as `Thread::create`.
///
/// # Panics
///
/// As `Thread::convert_to_port`.
fn thread_create(task: Option<&Reference<Task>>) -> Result<Reference<Port>, Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    // SAFETY: the handle keeps the task alive through the call, and the
    // thread's reference after; tasks are never freed yet.
    let thread = Thread::create(unsafe { Reference::as_ptr(task).as_ref() })?;
    // SAFETY: the reference its creation gave the caller, which the handle
    // takes.
    let thread = unsafe { Reference::from_raw(NonNull::from(thread)) };
    // A thread of the caller's task inherits from the caller, as pcb_init.
    if let Some(current) = smp::current().active_thread()
        && ptr::eq(current.task(), &raw const **task)
    {
        thread.machine_state().inherit();
    }
    // Its first run returns to user mode, as thread_bootstrap_return.
    thread.start(arch::MachineState::return_to_user);
    Ok(Thread::convert_to_port(thread))
}

/// Serves `vm_map`: `size` bytes of `memory_object` from `offset`, or a
/// copy of them, mapped into `map` at `address` or anywhere, with no bit of
/// `mask` set; gives where.
///
/// # Errors
///
/// `InvalidArgument` for no map or an inheritance Mach lacks; as
/// `vm::user::map`.
///
/// # Panics
///
/// For `VM_INHERIT_SHARE`: not yet. As `vm::user::map`.
#[expect(clippy::too_many_arguments, reason = "vm_map's parameters")]
fn vm_map(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    size: usize,
    mask: usize,
    anywhere: Boolean,
    memory_object: Option<&Reference<Port>>,
    offset: Offset,
    copy: Boolean,
    protection: u32,
    max_protection: u32,
    inheritance: u32,
) -> Result<VirtualAddress, Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    // VM_INHERIT_SHARE, VM_INHERIT_COPY, VM_INHERIT_NONE.
    let inheritance = match inheritance {
        0 => panic!("vm_map: VM_INHERIT_SHARE, not yet"),
        1 => Inheritance::Copy,
        2 => Inheritance::None,
        _ => return Err(Error::InvalidArgument),
    };
    let placement = if bool::from(anywhere) {
        Placement::Anywhere
    } else {
        Placement::Fixed(address)
    };
    let mapping = if bool::from(copy) {
        Mapping::Copy
    } else {
        Mapping::Object
    };
    vm::user::map(
        map,
        placement,
        size,
        mask,
        memory_object,
        offset,
        mapping,
        Protection::from_bits_retain(protection),
        Protection::from_bits_retain(max_protection),
        inheritance,
    )
}
