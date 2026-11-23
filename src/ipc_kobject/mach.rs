// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Mach's calls' server routines, as MIG makes them from `mach`'s
//! definitions; only those written so far.

use core::ptr::{self, NonNull};

use super::mig;
use super::mig::argument::OutOfLine;
use super::mig::boolean::Boolean;
use super::mig::port_array::PortArray;
use crate::arch;
use crate::error::Error;
use crate::host::Host;
use crate::ipc::port::port::Port;
use crate::ipc::port::send_right::{SendOnceRight, SendRight};
use crate::reference::Reference;
use crate::smp;
use crate::task::ports::{Memory, SpecialPort};
use crate::task::task::Task;
use crate::thread;
use crate::thread::machine::MachineThread as _;
use crate::thread::thread::Thread;
use crate::vm::map::change::{Change, Placement, Source};
use crate::vm::map::map::Map;
use crate::vm::object::object::Object;
use crate::vm::object::offset::Offset;
use crate::vm::object::state::{CopyStrategy, Flags};
use crate::vm::user::Mapping;
use crate::vm::{self, Inheritance, Protection, VirtualAddress};

mig::subsystem! {
    base 2000;
    7 task_create(parent_task = Port::to_task, inherit_memory: Boolean) -> (child_task: Port);
    8 task_terminate(target_task = Port::to_task);
    11 task_threads(target_task = Port::to_task) -> (thread_list: PortArray);
    12 task_info(target_task = Port::to_task, flavor: i32)
        -> (task_info_out: [i32; ..1024], CountInOut);
    16 thread_terminate(target_thread = Port::to_thread);
    17 thread_get_state(target_thread = Port::to_thread, flavor: i32)
        -> (old_state: [u32; ..1024], CountInOut);
    18 thread_set_state(target_thread = Port::to_thread, flavor: i32, new_state: [u32; ..1024]);
    19 thread_info(target_thread = Port::to_thread, flavor: i32)
        -> (thread_info_out: [i32; ..1024], CountInOut);
    21 vm_allocate(
        target_task = Port::task_map,
        address: VirtualAddress,
        size: usize,
        anywhere: Boolean
    ) -> (address: VirtualAddress);
    23 vm_deallocate(target_task = Port::task_map, address: VirtualAddress, size: usize);
    24 vm_protect(
        target_task = Port::task_map,
        address: VirtualAddress,
        size: usize,
        set_maximum: Boolean,
        new_protection: u32
    );
    25 vm_inherit(
        target_task = Port::task_map,
        address: VirtualAddress,
        size: usize,
        new_inheritance: u32
    );
    26 vm_read(
        target_task = Port::task_map,
        address: VirtualAddress,
        size: usize
    ) -> (data: OutOfLine<u8>);
    27 vm_write(target_task = Port::task_map, address: VirtualAddress, data: OutOfLine<u8>);
    28 vm_copy(
        target_task = Port::task_map,
        source_address: VirtualAddress,
        size: usize,
        dest_address: VirtualAddress
    );
    29 vm_region(target_task = Port::task_map, address: VirtualAddress) -> (
        address: VirtualAddress,
        size: usize,
        protection: u32,
        max_protection: u32,
        inheritance: u32,
        is_shared: Boolean,
        object_name: Port,
        offset: usize
    );
    30 vm_statistics(target_task = Port::task_map) -> (vm_stats: [i32; 13]);
    39 memory_object_data_unavailable(
        control = Port::to_vm_object,
        offset: Offset,
        size: usize
    );
    40 memory_object_get_attributes(control = Port::to_vm_object)
        -> (object_ready: Boolean, may_cache: Boolean, copy_strategy: i32);
    41 vm_set_default_memory_manager(host = Port::as_privileged_host, default_manager: SendRight)
        -> (default_manager: PortMaybe);
    44 memory_object_lock_request(
        control = Port::to_vm_object,
        offset: Offset,
        size: usize,
        should_return: i32,
        should_flush: Boolean,
        lock_value: i32,
        reply_to: SendOnceRight
    );
    90 memory_object_data_error(
        control = Port::to_vm_object,
        offset: Offset,
        size: usize,
        error_value: i32
    );
    93 memory_object_data_supply(
        control = Port::to_vm_object,
        offset: Offset,
        data: OutOfLine<u8>,
        lock_value: u32,
        precious: Boolean,
        reply_to: SendOnceRight
    );
    94 memory_object_ready(control = Port::to_vm_object, may_cache: Boolean, copy_strategy: i32);
    95 memory_object_change_attributes(
        control = Port::to_vm_object,
        may_cache: Boolean,
        copy_strategy: i32,
        reply_to: SendOnceRight
    );
    56 task_suspend(target_task = Port::to_task);
    57 task_resume(target_task = Port::to_task);
    59 task_set_special_port(task = Port::to_task, which_port: i32, special_port: SendRight);
    61 thread_create(parent_task = Port::to_task) -> (child_thread: Port);
    62 thread_suspend(target_thread = Port::to_thread);
    63 thread_resume(target_thread = Port::to_thread);
    64 thread_abort(target_thread = Port::to_thread);
    68 thread_set_special_port(thread = Port::to_thread, which_port: i32, special_port: SendRight);
    89 vm_map(
        target_task = Port::task_map,
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

/// Serves `task_create`: a new task with `task` as its parent, over a copy
/// of its memory for `inherit_memory`; gives a send right for its port.
///
/// # Errors
///
/// `InvalidTask` for no task; as `Task::create`.
///
/// # Panics
///
/// As `Task::create` and `Task::convert_to_port`.
fn task_create(
    task: Option<&Reference<Task>>,
    inherit_memory: Boolean,
) -> Result<Reference<Port>, Error> {
    let Some(task) = task else {
        return Err(Error::InvalidTask);
    };
    let memory = if bool::from(inherit_memory) {
        Memory::Inherited
    } else {
        Memory::Empty
    };
    let child = Task::create(Some(task), memory)?;
    Ok(Task::convert_to_port(child))
}

/// Serves `task_terminate`: ends `task`.
///
/// # Errors
///
/// `InvalidArgument` for no task.
///
/// # Panics
///
/// As `Task::terminate`.
fn task_terminate(task: Option<&Reference<Task>>) -> Result<(), Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    task.terminate();
    Ok(())
}

/// Serves `task_threads`: send rights for `task`'s threads' ports, in an
/// out-of-line array.
///
/// # Errors
///
/// `InvalidArgument` for no task; as `Task::thread_ports`.
///
/// # Panics
///
/// As `Task::thread_ports`.
fn task_threads(task: Option<&Reference<Task>>) -> Result<PortArray, Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    let ports = task.thread_ports()?;
    // SAFETY: a kalloc array of its size holding ports' exposed addresses,
    // each with its reference, as `Task::thread_ports` gives it.
    Ok(unsafe { PortArray::new(ports) })
}

/// Serves `task_info`: up to `info.len()` integers of `flavor` for `task`;
/// gives how many.
///
/// # Errors
///
/// `InvalidArgument` for no task or a flavor Mach lacks; as
/// `Task::basic_info`, `Task::events_info` and `Task::thread_times_info`.
fn task_info(
    task: Option<&Reference<Task>>,
    flavor: i32,
    info: &mut [i32],
) -> Result<usize, Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    // TASK_BASIC_INFO, TASK_EVENTS_INFO, TASK_THREAD_TIMES_INFO.
    match flavor {
        1 => task.basic_info(info),
        2 => task.events_info(info),
        3 => task.thread_times_info(info),
        _ => Err(Error::InvalidArgument),
    }
}

/// Serves `thread_terminate`: ends `thread`.
///
/// # Errors
///
/// `InvalidArgument` for no thread; as `Thread::terminate`.
///
/// # Panics
///
/// As `Thread::terminate`.
fn thread_terminate(thread: Option<&Reference<Thread>>) -> Result<(), Error> {
    let Some(thread) = thread else {
        return Err(Error::InvalidArgument);
    };
    thread.terminate()
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

/// Serves `thread_set_state`: sets `thread`'s machine state of `flavor`
/// from `state`.
///
/// # Errors
///
/// As `Thread::write_state`.
///
/// # Panics
///
/// As `Thread::write_state`.
fn thread_set_state(
    thread: Option<&Reference<Thread>>,
    flavor: i32,
    state: &[u32],
) -> Result<(), Error> {
    // SAFETY: the handle keeps the thread alive through the call; threads
    // are never freed yet.
    let thread = thread.map(|thread| unsafe { Reference::as_ptr(thread).as_ref() });
    Thread::write_state(thread, flavor, state)
}

/// Serves `thread_info`: up to `info.len()` integers of `flavor` for
/// `thread`; gives how many.
///
/// # Errors
///
/// `InvalidArgument` for no thread or a flavor Mach lacks; as
/// `Thread::basic_info` and `Thread::sched_info`.
///
/// # Panics
///
/// As `Thread::sched_info`.
fn thread_info(
    thread: Option<&Reference<Thread>>,
    flavor: i32,
    info: &mut [i32],
) -> Result<usize, Error> {
    let Some(thread) = thread else {
        return Err(Error::InvalidArgument);
    };
    // THREAD_BASIC_INFO, THREAD_SCHED_INFO.
    match flavor {
        1 => thread.basic_info(info),
        2 => thread.sched_info(info),
        _ => Err(Error::InvalidArgument),
    }
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

/// Serves `memory_object_get_attributes`: whether the object whose control
/// port it is has a ready pager, whether it may cache, and its copy
/// strategy.
///
/// # Errors
///
/// `InvalidArgument` if the port is no object's control port.
fn memory_object_get_attributes(
    object: Option<&Reference<Object>>,
) -> Result<(Boolean, Boolean, i32), Error> {
    let Some(object) = object else {
        return Err(Error::InvalidArgument);
    };
    let (ready, may_cache, copy_strategy) = object.attributes();
    Ok((
        Boolean::from(ready),
        Boolean::from(may_cache),
        copy_strategy,
    ))
}

/// Serves `memory_object_ready`: the object whose control port it is has a
/// ready pager, may cache as `may_cache` says, and copies as
/// `copy_strategy` says.
///
/// # Errors
///
/// `InvalidArgument` if the port is no object's control port, or the
/// strategy is not one Mach names.
fn memory_object_ready(
    object: Option<&Reference<Object>>,
    may_cache: Boolean,
    copy_strategy: i32,
) -> Result<(), Error> {
    let Some(object) = object else {
        return Err(Error::InvalidArgument);
    };
    let Some(copy_strategy) = CopyStrategy::from_number(copy_strategy) else {
        return Err(Error::InvalidArgument);
    };
    let mut state = object.lock();
    // As memory_object_set_attributes_common.
    if bool::from(may_cache) {
        state.flags.insert(Flags::CAN_PERSIST);
    } else {
        state.flags.remove(Flags::CAN_PERSIST);
    }
    state.flags.insert(Flags::PAGER_READY);
    state.copy_strategy = copy_strategy;
    drop(state);
    // A fault waiting for it to be ready goes on.
    crate::sched::wait::wakeup(crate::sched::event::Event::of(&**object));
    Ok(())
}

/// Serves `memory_object_data_unavailable`: the pages of the object whose
/// control port it is, `size` bytes from `offset`, have no data, so they
/// are filled with zeros at their faults.
///
/// # Errors
///
/// `InvalidArgument` if the port is no object's control port, or `size` is
/// not a whole number of pages.
///
/// # Panics
///
/// As `pager::data_unavailable`.
fn memory_object_data_unavailable(
    object: Option<&Reference<Object>>,
    offset: Offset,
    size: usize,
) -> Result<(), Error> {
    let Some(object) = object else {
        return Err(Error::InvalidArgument);
    };
    vm::pager::data_unavailable(object, usize::from(offset), size)
}

/// Serves `memory_object_data_error`: the pages of the object whose control
/// port it is, `size` bytes from `offset`, are in `error_value`'s error, so
/// faults on them fail.
///
/// # Errors
///
/// `InvalidArgument` if the port is no object's control port, or `size` is
/// not a whole number of pages.
///
/// # Panics
///
/// As `pager::data_error`.
fn memory_object_data_error(
    object: Option<&Reference<Object>>,
    offset: Offset,
    size: usize,
    _error_value: i32,
) -> Result<(), Error> {
    let Some(object) = object else {
        return Err(Error::InvalidArgument);
    };
    vm::pager::data_error(object, usize::from(offset), size)
}

/// Serves `memory_object_data_supply`: `data`, `count` bytes, is the data
/// of the pages of the object whose control port it is from `offset`, held
/// from the accesses in `lock_value` and `precious` to its pager; tells
/// `reply_to`, a send-once right, how it went.
///
/// # Errors
///
/// `InvalidArgument` if the port is no object's control port, `lock_value`
/// names accesses Mach lacks, or the data is not a whole number of pages.
///
/// # Panics
///
/// As `pager::data_supply`.
fn memory_object_data_supply(
    object: Option<&Reference<Object>>,
    offset: Offset,
    data: Option<OutOfLine<u8>>,
    lock_value: u32,
    precious: Boolean,
    reply_to: Option<SendOnceRight>,
) -> Result<(), Error> {
    let Some(object) = object else {
        return Err(Error::InvalidArgument);
    };
    let Some(data) = data else {
        return Err(Error::InvalidArgument);
    };
    let count = data.count();
    let data = data.into_copy();
    vm::pager::data_supply(
        object,
        usize::from(offset),
        &data,
        count,
        lock_value,
        bool::from(precious),
        reply_to.map(SendOnceRight::into_reference),
    )
}

/// Serves `memory_object_lock_request`: the pages of the object whose
/// control port it is, `size` bytes from `offset`, are held from
/// `lock_value`'s accesses, those among them `should_return` wants are
/// given back to its pager, those `should_flush` says are dropped, and
/// `reply_to`, a send-once right, hears that it is done.
///
/// # Errors
///
/// `InvalidArgument` if the port is no object's control port, or
/// `lock_value` names accesses Mach lacks.
///
/// # Panics
///
/// As `pager::lock_request`.
fn memory_object_lock_request(
    object: Option<&Reference<Object>>,
    offset: Offset,
    size: usize,
    should_return: i32,
    should_flush: Boolean,
    lock_value: i32,
    reply_to: Option<SendOnceRight>,
) -> Result<(), Error> {
    let Some(object) = object else {
        return Err(Error::InvalidArgument);
    };
    vm::pager::lock_request(
        object,
        usize::from(offset),
        size,
        should_return,
        bool::from(should_flush),
        lock_value,
        reply_to.map(SendOnceRight::into_reference),
    )
}

/// Serves `memory_object_change_attributes`: the object whose control port
/// it is may cache as `may_cache` says and copies as `copy_strategy` says;
/// `reply_to`, a send-once right, hears it.
///
/// # Errors
///
/// `InvalidArgument` if the port is no object's control port, or the
/// strategy is not one Mach names.
///
/// # Panics
///
/// As `pager::change_attributes`.
fn memory_object_change_attributes(
    object: Option<&Reference<Object>>,
    may_cache: Boolean,
    copy_strategy: i32,
    reply_to: Option<SendOnceRight>,
) -> Result<(), Error> {
    let Some(object) = object else {
        return Err(Error::InvalidArgument);
    };
    let Some(copy_strategy) = CopyStrategy::from_number(copy_strategy) else {
        return Err(Error::InvalidArgument);
    };
    vm::pager::change_attributes(
        object,
        bool::from(may_cache),
        copy_strategy,
        reply_to.map(SendOnceRight::into_reference),
    );
    Ok(())
}

/// Serves `vm_set_default_memory_manager`: `default_manager`, which it
/// keeps, the host's default memory manager; gives the previous one's send
/// right, or a send right for the current one for none.
///
/// # Errors
///
/// `InvalidHost` unless `host` is the privileged host.
///
/// # Panics
///
/// If the current manager is dead: not yet.
fn vm_set_default_memory_manager(
    host: Option<&&Host>,
    default_manager: Option<SendRight>,
) -> Result<Option<Reference<Port>>, Error> {
    if host.is_none() {
        return Err(Error::InvalidHost);
    }
    Ok(vm::user::set_default_memory_manager(default_manager))
}

/// Serves `vm_statistics`: the host's page statistics.
///
/// # Errors
///
/// `InvalidArgument` for no map.
fn vm_statistics(map: Option<&Reference<Map>>) -> Result<[i32; 13], Error> {
    if map.is_none() {
        return Err(Error::InvalidArgument);
    }
    Ok(vm::user::statistics())
}

/// Serves `vm_inherit`: what child tasks get of `size` bytes of `map` from
/// `address`, as `new_inheritance` says.
///
/// # Errors
///
/// `InvalidArgument` for no map or inheritance; as `Map::inherit`.
///
/// # Panics
///
/// For `VM_INHERIT_SHARE`: not yet. As `Map::inherit`.
fn vm_inherit(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    size: usize,
    new_inheritance: u32,
) -> Result<(), Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    let Some(inheritance) = Inheritance::from_number(new_inheritance) else {
        panic!("vm_inherit: VM_INHERIT_SHARE, not yet");
    };
    // As trunc_page and round_page.
    let start = address.align_down(arch::PAGE_SIZE);
    let end = VirtualAddress::from(usize::from(address).wrapping_add(size))
        .next_multiple_of(arch::PAGE_SIZE);
    map.inherit(start..end, inheritance)
}

/// Serves `vm_read`: `size` bytes of `map` from `address`, in an out-of-line
/// copy.
///
/// # Errors
///
/// `InvalidArgument` for no map.
///
/// # Panics
///
/// As `Map::copy_in`.
fn vm_read(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
    size: usize,
) -> Result<OutOfLine<u8>, Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    if size == 0 {
        return Ok(OutOfLine::empty());
    }
    let copy = map.copy_in(address..address + size, Source::Keep);
    Ok(OutOfLine::new(copy, size))
}

/// Serves `vm_copy`: `size` bytes of `map` from `source` copied over it at
/// `destination`.
///
/// # Errors
///
/// `InvalidArgument` for no map; as `vm::user::copy`.
///
/// # Panics
///
/// As `vm::user::copy`.
fn vm_copy(
    map: Option<&Reference<Map>>,
    source: VirtualAddress,
    size: usize,
    destination: VirtualAddress,
) -> Result<(), Error> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    vm::user::copy(map, source, size, destination)
}

/// Serves `vm_region`: what `map` maps at or after `address`, starting with
/// the region's own address, and a send right for its object's name port.
///
/// # Errors
///
/// `InvalidArgument` for no map, and `NoSpace` if no entry lies at or after
/// `address`; as `Map::region`.
#[expect(clippy::type_complexity, reason = "vm_region's outs")]
fn vm_region(
    map: Option<&Reference<Map>>,
    address: VirtualAddress,
) -> Result<
    (
        VirtualAddress,
        usize,
        u32,
        u32,
        u32,
        Boolean,
        Reference<Port>,
        usize,
    ),
    Error,
> {
    let Some(map) = map else {
        return Err(Error::InvalidArgument);
    };
    let region = map.region(address)?;
    let object_name = region
        .object_name
        .unwrap_or_else(|| panic!("vm_region: an object with no name port, not yet"));
    Ok((
        region.start,
        region.end - region.start,
        region.protection.bits(),
        region.max_protection.bits(),
        u32::from(region.inheritance.number()),
        Boolean::from(false),
        object_name,
        usize::from(region.offset),
    ))
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

/// Serves `task_suspend`: stops `task` once more for users.
///
/// # Errors
///
/// `InvalidArgument` for no task.
///
/// # Panics
///
/// As `Task::suspend`.
fn task_suspend(task: Option<&Reference<Task>>) -> Result<(), Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    task.suspend();
    Ok(())
}

/// Serves `task_resume`: lifts one stop users asked for on `task`.
///
/// # Errors
///
/// `InvalidArgument` for no task; as `Task::resume`.
///
/// # Panics
///
/// As `Task::resume`.
fn task_resume(task: Option<&Reference<Task>>) -> Result<(), Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    task.resume()
}

/// Serves `task_set_special_port`: `task`'s special port numbered
/// `which_port` becomes `special_port`.
///
/// # Errors
///
/// `InvalidArgument` for no task; the right is released, as
/// `ipc_kmsg_destroy` of the request does.
///
/// # Panics
///
/// If `which_port` names no special port: not yet. As
/// `Task::set_special_port`.
fn task_set_special_port(
    task: Option<&Reference<Task>>,
    which_port: i32,
    special_port: Option<SendRight>,
) -> Result<(), Error> {
    let Some(task) = task else {
        return Err(Error::InvalidArgument);
    };
    let which = SpecialPort::try_from(which_port).unwrap_or_else(|number| {
        panic!("task_set_special_port: KERN_INVALID_ARGUMENT for {number}, not yet")
    });
    task.set_special_port(which, special_port);
    Ok(())
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

/// Serves `thread_suspend`: stops `thread` once more for users.
///
/// # Errors
///
/// `InvalidArgument` for no thread.
///
/// # Panics
///
/// As `Thread::suspend`.
fn thread_suspend(thread: Option<&Reference<Thread>>) -> Result<(), Error> {
    let Some(thread) = thread else {
        return Err(Error::InvalidArgument);
    };
    thread.suspend();
    Ok(())
}

/// Serves `thread_resume`: lifts one stop users asked for on `thread`.
///
/// # Errors
///
/// `InvalidArgument` for no thread; as `Thread::resume`.
///
/// # Panics
///
/// As `sched::make_runnable`, through `Thread::resume`.
fn thread_resume(thread: Option<&Reference<Thread>>) -> Result<(), Error> {
    let Some(thread) = thread else {
        return Err(Error::InvalidArgument);
    };
    // SAFETY: the handle keeps the thread alive through the call, and its
    // own reference after; threads are never freed yet.
    unsafe { Reference::as_ptr(thread).as_ref() }.resume()
}

/// Serves `thread_abort`: ends `thread`'s interruptible wait at a clean
/// point.
///
/// # Errors
///
/// As `Thread::abort`.
///
/// # Panics
///
/// As `Thread::abort`.
fn thread_abort(thread: Option<&Reference<Thread>>) -> Result<(), Error> {
    // SAFETY: the handle keeps the thread alive through the call; a run
    // queue holds it after only while it lives, as its free waits for it to
    // stop.
    let thread = thread.map(|thread| unsafe { Reference::as_ptr(thread).as_ref() });
    Thread::abort(thread)
}

/// Serves `thread_set_special_port`: `thread`'s special port numbered
/// `which_port` becomes `special_port`.
///
/// # Errors
///
/// `InvalidArgument` for no thread, or if `which_port` names no special
/// port; as `Thread::set_special_port`. The right is released, as
/// `ipc_kmsg_destroy` of the request does.
///
/// # Panics
///
/// As `Thread::set_special_port`.
fn thread_set_special_port(
    thread: Option<&Reference<Thread>>,
    which_port: i32,
    special_port: Option<SendRight>,
) -> Result<(), Error> {
    let Some(thread) = thread else {
        return Err(Error::InvalidArgument);
    };
    let Ok(which) = thread::ports::SpecialPort::try_from(which_port) else {
        return Err(Error::InvalidArgument);
    };
    thread.set_special_port(which, special_port)
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
