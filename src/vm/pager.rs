// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The kernel's calls to pagers outside it, as `memory_object` and
//! `memory_object_default` send them.

use core::mem::size_of;
use core::ptr;

use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::ipc::MessageType;
use crate::ipc::kernel_message::KernelMessage;
use crate::ipc::kernel_message::header::{Header, MessageBits};
use crate::ipc::mach_msg;
use crate::ipc::port::port::Port;
use crate::ipc_kobject::mig::argument::{out_of_line_word, type_word};
use crate::kalloc;
use crate::reference::Reference;
use crate::sched::event::Event;
use crate::sched::wait;
use crate::slab::CacheBox;
use crate::vm::map::change::{Placement, RangeCheck, Source};
use crate::vm::map::copy::MapCopy;
use crate::vm::object::object::Object;
use crate::vm::object::state::{CopyStrategy, Flags};
use crate::vm::page::page::Page;
use crate::vm::{Protection, page, user};

/// `memory_object_create`'s message id.
const MEMORY_OBJECT_CREATE: i32 = 2250;

/// `memory_object_init`'s message id.
const MEMORY_OBJECT_INIT: i32 = 2200;

/// Its request: the new object's receive right, its size, the control and
/// name ports' send rights, and the page size.
#[repr(C)]
struct Create {
    /// Its header.
    header: Header,
    /// The object's type word.
    object_word: u64,
    /// Its receive right.
    object: usize,
    /// The size's type word.
    size_word: u64,
    /// Its size.
    size: usize,
    /// The control port's type word.
    control_word: u64,
    /// Its send right.
    control: usize,
    /// The name port's type word.
    name_word: u64,
    /// Its send right.
    name: usize,
    /// The page size's type word.
    page_word: u64,
    /// Its page size.
    page_size: usize,
}

const _: () = assert!(size_of::<Create>() == 112);

/// Its request: the control and name ports' send rights, and the page
/// size.
#[repr(C)]
struct Init {
    /// Its header.
    header: Header,
    /// The control port's type word.
    control_word: u64,
    /// Its send right.
    control: usize,
    /// The name port's type word.
    name_word: u64,
    /// Its send right.
    name: usize,
    /// The page size's type word.
    page_word: u64,
    /// Its page size.
    page_size: usize,
}

const _: () = assert!(size_of::<Init>() == 80);

/// Sends `memory_object_init` to `pager` for its object, with the `control`
/// and `name` ports' send rights and `page_size`.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn memory_object_init(
    pager: Reference<Port>,
    control: Reference<Port>,
    name: Reference<Port>,
    page_size: usize,
) {
    let mut message =
        KernelMessage::allocate_saved().unwrap_or_else(|_| panic!("memory_object_init: no buffer"));
    // SAFETY: `Init` is `repr(C)`, a `Header`, then integers and ports'
    // addresses only.
    let request = unsafe { message.layout_mut::<Init>() };
    let bits = MessageBits::new(Some(MessageType::PortSend), None).with_complex();
    *request = Init {
        header: Header {
            bits,
            size: u32::try_from(size_of::<Init>()).unwrap(),
            remote_port: Reference::into_raw(pager).as_ptr().expose_provenance(),
            local_port: 0,
            sequence_number: 0,
            id: MEMORY_OBJECT_INIT,
        },
        control_word: type_word(MessageType::PortSend, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        name_word: type_word(MessageType::PortSend, 64, 1),
        name: Reference::into_raw(name).as_ptr().expose_provenance(),
        page_word: type_word(MessageType::Integer64, 64, 1),
        page_size,
    };
    mach_msg::send::send_always(message);
}

/// Sends `memory_object_create` to `manager` for `object`, the new pager's
/// receive right, its `size`, the `control` and `name` ports' send rights
/// and `page_size`, as `vm_object_pager_create` tells the default manager.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn memory_object_create(
    manager: Reference<Port>,
    object: Reference<Port>,
    size: usize,
    control: Reference<Port>,
    name: Reference<Port>,
    page_size: usize,
) {
    let mut message = KernelMessage::allocate_saved()
        .unwrap_or_else(|_| panic!("memory_object_create: no buffer"));
    // SAFETY: `Create` is `repr(C)`, a `Header`, then integers and ports'
    // addresses only.
    let request = unsafe { message.layout_mut::<Create>() };
    let bits = MessageBits::new(Some(MessageType::PortSend), None).with_complex();
    *request = Create {
        header: Header {
            bits,
            size: u32::try_from(size_of::<Create>()).unwrap(),
            remote_port: Reference::into_raw(manager).as_ptr().expose_provenance(),
            local_port: 0,
            sequence_number: 0,
            id: MEMORY_OBJECT_CREATE,
        },
        object_word: type_word(MessageType::PortReceive, 64, 1),
        object: Reference::into_raw(object).as_ptr().expose_provenance(),
        size_word: type_word(MessageType::Integer64, 64, 1),
        size,
        control_word: type_word(MessageType::PortSend, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        name_word: type_word(MessageType::PortSend, 64, 1),
        name: Reference::into_raw(name).as_ptr().expose_provenance(),
        page_word: type_word(MessageType::Integer64, 64, 1),
        page_size,
    };
    mach_msg::send::send_always(message);
}

/// `memory_object_terminate`'s message id.
const MEMORY_OBJECT_TERMINATE: i32 = 2201;

/// `memory_object_copy`'s message id.
const MEMORY_OBJECT_COPY: i32 = 2202;

/// `memory_object_data_request`'s message id.
const MEMORY_OBJECT_DATA_REQUEST: i32 = 2203;

/// `memory_object_data_unlock`'s message id.
const MEMORY_OBJECT_DATA_UNLOCK: i32 = 2204;

/// `memory_object_lock_completed`'s message id.
const MEMORY_OBJECT_LOCK_COMPLETED: i32 = 2206;

/// `memory_object_supply_completed`'s message id.
const MEMORY_OBJECT_SUPPLY_COMPLETED: i32 = 2207;

/// `memory_object_data_return`'s message id.
const MEMORY_OBJECT_DATA_RETURN: i32 = 2208;

/// `memory_object_change_completed`'s message id.
const MEMORY_OBJECT_CHANGE_COMPLETED: i32 = 2209;

/// `memory_object_terminate`'s request: the control and name ports'
/// receive rights.
#[repr(C)]
struct Terminate {
    /// Its header.
    header: Header,
    /// The control port's type word.
    control_word: u64,
    /// Its receive right.
    control: usize,
    /// The name port's type word.
    name_word: u64,
    /// Its receive right.
    name: usize,
}

const _: () = assert!(size_of::<Terminate>() == 64);

/// `memory_object_copy`'s request: the old control port's send right, the
/// range, and the new object's receive right.
#[repr(C)]
struct Copy {
    /// Its header.
    header: Header,
    /// The control port's type word.
    control_word: u64,
    /// Its send right.
    control: usize,
    /// The offset's type word.
    offset_word: u64,
    /// Its offset.
    offset: usize,
    /// The length's type word.
    length_word: u64,
    /// Its length.
    length: usize,
    /// The new object's type word.
    object_word: u64,
    /// Its receive right.
    object: usize,
}

const _: () = assert!(size_of::<Copy>() == 96);

/// `memory_object_data_request`'s and `memory_object_data_unlock`'s
/// request: the control port's send right, the range and the access.
#[repr(C)]
struct Request {
    /// Its header.
    header: Header,
    /// The control port's type word.
    control_word: u64,
    /// Its send right.
    control: usize,
    /// The offset's type word.
    offset_word: u64,
    /// Its offset.
    offset: usize,
    /// The length's type word.
    length_word: u64,
    /// Its length.
    length: usize,
    /// The access's type word.
    access_word: u64,
    /// It, and padding to 8 bytes.
    access: [u8; 8],
}

const _: () = assert!(size_of::<Request>() == 96);

/// `memory_object_lock_completed`'s request: the control port's send right
/// and the range.
#[repr(C)]
struct LockCompleted {
    /// Its header.
    header: Header,
    /// The control port's type word.
    control_word: u64,
    /// Its send right.
    control: usize,
    /// The offset's type word.
    offset_word: u64,
    /// Its offset.
    offset: usize,
    /// The length's type word.
    length_word: u64,
    /// Its length.
    length: usize,
}

const _: () = assert!(size_of::<LockCompleted>() == 80);

/// `memory_object_supply_completed`'s request: the control port's send
/// right, the range and the result.
#[repr(C)]
struct SupplyCompleted {
    /// Its header.
    header: Header,
    /// The control port's type word.
    control_word: u64,
    /// Its send right.
    control: usize,
    /// The offset's type word.
    offset_word: u64,
    /// Its offset.
    offset: usize,
    /// The length's type word.
    length_word: u64,
    /// Its length.
    length: usize,
    /// The result's type word.
    result_word: u64,
    /// It, and padding to 8 bytes.
    result: [u8; 8],
    /// The error offset's type word.
    error_word: u64,
    /// Its offset.
    error_offset: usize,
}

const _: () = assert!(size_of::<SupplyCompleted>() == 112);

/// `memory_object_data_return`'s request: the control port's send right,
/// the offset, the data out of line, and the flags.
#[repr(C)]
struct DataReturn {
    /// Its header.
    header: Header,
    /// The control port's type word.
    control_word: u64,
    /// Its send right.
    control: usize,
    /// The offset's type word.
    offset_word: u64,
    /// Its offset.
    offset: usize,
    /// The data's type word.
    data_word: u64,
    /// Its address.
    data: usize,
    /// The dirty flag's type word.
    dirty_word: u64,
    /// It, and padding to 8 bytes.
    dirty: [u8; 8],
    /// The kernel-copy flag's type word.
    kernel_copy_word: u64,
    /// It, and padding to 8 bytes.
    kernel_copy: [u8; 8],
}

const _: () = assert!(size_of::<DataReturn>() == 112);

/// `memory_object_change_completed`'s request: whether the object may
/// cache, and how it copies.
#[repr(C)]
struct ChangeCompleted {
    /// Its header.
    header: Header,
    /// The may-cache flag's type word.
    may_cache_word: u64,
    /// It, and padding to 8 bytes.
    may_cache: [u8; 8],
    /// The strategy's type word.
    strategy_word: u64,
    /// It, and padding to 8 bytes.
    strategy: [u8; 8],
}

const _: () = assert!(size_of::<ChangeCompleted>() == 64);

/// Its header: to `port`, a right of `kind`, `size` bytes, `id`.
fn header(kind: MessageType, port: Reference<Port>, size: usize, id: i32) -> Header {
    Header {
        bits: MessageBits::new(Some(kind), None).with_complex(),
        size: u32::try_from(size).unwrap(),
        remote_port: Reference::into_raw(port).as_ptr().expose_provenance(),
        local_port: 0,
        sequence_number: 0,
        id,
    }
}

/// A 32-bit integer's type word and its data, padded to 8 bytes.
fn integer32(value: i32) -> (u64, [u8; 8]) {
    let mut data = [0; 8];
    data[..4].copy_from_slice(&value.to_le_bytes());
    (type_word(MessageType::Integer32, 32, 1), data)
}

/// A boolean's type word and its data, padded to 8 bytes.
fn boolean(value: bool) -> (u64, [u8; 8]) {
    let mut data = [0; 8];
    data[..4].copy_from_slice(&u32::from(value).to_le_bytes());
    (type_word(MessageType::Boolean, 32, 1), data)
}

/// Sends `memory_object_terminate` to `pager`, giving it the `control` and
/// `name` ports' receive rights, as `memory_object_release` does.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn memory_object_terminate(
    pager: Reference<Port>,
    control: Reference<Port>,
    name: Reference<Port>,
) {
    // The kernel's receive rights leave with the message.
    control.clear_receiver_limbo();
    name.clear_receiver_limbo();
    let mut message = KernelMessage::allocate_saved()
        .unwrap_or_else(|_| panic!("memory_object_terminate: no buffer"));
    // SAFETY: `Terminate` is `repr(C)`, a `Header`, then integers and ports'
    // addresses only.
    let request = unsafe { message.layout_mut::<Terminate>() };
    *request = Terminate {
        header: header(
            MessageType::PortSend,
            pager,
            size_of::<Terminate>(),
            MEMORY_OBJECT_TERMINATE,
        ),
        control_word: type_word(MessageType::PortReceive, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        name_word: type_word(MessageType::PortReceive, 64, 1),
        name: Reference::into_raw(name).as_ptr().expose_provenance(),
    };
    mach_msg::send::send_always(message);
}

/// Sends `memory_object_copy` to `pager` for `length` bytes of its object
/// from `offset`, with the `control` port's send right and `object`, the
/// new object's receive right, as `vm_object_copy_call` does.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn memory_object_copy(
    pager: Reference<Port>,
    control: Reference<Port>,
    offset: usize,
    length: usize,
    object: Reference<Port>,
) {
    let mut message =
        KernelMessage::allocate_saved().unwrap_or_else(|_| panic!("memory_object_copy: no buffer"));
    // SAFETY: `Copy` is `repr(C)`, a `Header`, then integers and ports'
    // addresses only.
    let request = unsafe { message.layout_mut::<Copy>() };
    *request = Copy {
        header: header(
            MessageType::PortSend,
            pager,
            size_of::<Copy>(),
            MEMORY_OBJECT_COPY,
        ),
        control_word: type_word(MessageType::PortSend, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        offset_word: type_word(MessageType::Integer64, 64, 1),
        offset,
        length_word: type_word(MessageType::Integer64, 64, 1),
        length,
        object_word: type_word(MessageType::PortReceive, 64, 1),
        object: Reference::into_raw(object).as_ptr().expose_provenance(),
    };
    mach_msg::send::send_always(message);
}

/// Sends `memory_object_data_request` to `pager` for `length` bytes of its
/// object from `offset`, with the `control` port's send right and the
/// `access` wanted, as a fault does.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn data_request(
    pager: Reference<Port>,
    control: Reference<Port>,
    offset: usize,
    length: usize,
    access: i32,
) {
    request(
        MEMORY_OBJECT_DATA_REQUEST,
        pager,
        control,
        offset,
        length,
        access,
    );
}

/// Sends `memory_object_data_unlock` to `pager` for `length` bytes of its
/// object from `offset`, with the `control` port's send right and the
/// `access` wanted, as a fault held off by a page lock does.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn data_unlock(
    pager: Reference<Port>,
    control: Reference<Port>,
    offset: usize,
    length: usize,
    access: i32,
) {
    request(
        MEMORY_OBJECT_DATA_UNLOCK,
        pager,
        control,
        offset,
        length,
        access,
    );
}

/// Sends a request with `id` to `pager`, as `memory_object_data_request`
/// and `memory_object_data_unlock` are.
///
/// # Panics
///
/// If no buffer can be allocated.
fn request(
    id: i32,
    pager: Reference<Port>,
    control: Reference<Port>,
    offset: usize,
    length: usize,
    access: i32,
) {
    let mut message =
        KernelMessage::allocate_saved().unwrap_or_else(|_| panic!("a pager request: no buffer"));
    let (access_word, access) = integer32(access);
    // SAFETY: `Request` is `repr(C)`, a `Header`, then integers and ports'
    // addresses only.
    let request = unsafe { message.layout_mut::<Request>() };
    *request = Request {
        header: header(MessageType::PortSend, pager, size_of::<Request>(), id),
        control_word: type_word(MessageType::PortSend, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        offset_word: type_word(MessageType::Integer64, 64, 1),
        offset,
        length_word: type_word(MessageType::Integer64, 64, 1),
        length,
        access_word,
        access,
    };
    mach_msg::send::send_always(message);
}

/// Sends `memory_object_lock_completed` to `notification`, a send-once
/// right, for `length` bytes of its object from `offset`, with the
/// `control` port's send right, as a lock request's completion does.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn lock_completed(
    notification: Reference<Port>,
    control: Reference<Port>,
    offset: usize,
    length: usize,
) {
    let mut message = KernelMessage::allocate_saved()
        .unwrap_or_else(|_| panic!("memory_object_lock_completed: no buffer"));
    // SAFETY: `LockCompleted` is `repr(C)`, a `Header`, then integers and
    // ports' addresses only.
    let request = unsafe { message.layout_mut::<LockCompleted>() };
    *request = LockCompleted {
        header: header(
            MessageType::PortSendOnce,
            notification,
            size_of::<LockCompleted>(),
            MEMORY_OBJECT_LOCK_COMPLETED,
        ),
        control_word: type_word(MessageType::PortSend, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        offset_word: type_word(MessageType::Integer64, 64, 1),
        offset,
        length_word: type_word(MessageType::Integer64, 64, 1),
        length,
    };
    mach_msg::send::send_always(message);
}

/// Sends `memory_object_supply_completed` to `notification`, a send-once
/// right, for `length` bytes of its object from `offset`, with the
/// `control` port's send right, the `result` and where it went wrong, as
/// `memory_object_data_supply` replies.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn supply_completed(
    notification: Reference<Port>,
    control: Reference<Port>,
    offset: usize,
    length: usize,
    result: i32,
    error_offset: usize,
) {
    let mut message = KernelMessage::allocate_saved()
        .unwrap_or_else(|_| panic!("memory_object_supply_completed: no buffer"));
    let (result_word, result) = integer32(result);
    // SAFETY: `SupplyCompleted` is `repr(C)`, a `Header`, then integers and
    // ports' addresses only.
    let request = unsafe { message.layout_mut::<SupplyCompleted>() };
    *request = SupplyCompleted {
        header: header(
            MessageType::PortSendOnce,
            notification,
            size_of::<SupplyCompleted>(),
            MEMORY_OBJECT_SUPPLY_COMPLETED,
        ),
        control_word: type_word(MessageType::PortSend, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        offset_word: type_word(MessageType::Integer64, 64, 1),
        offset,
        length_word: type_word(MessageType::Integer64, 64, 1),
        length,
        result_word,
        result,
        error_word: type_word(MessageType::Integer64, 64, 1),
        error_offset,
    };
    mach_msg::send::send_always(message);
}

/// Sends `memory_object_data_return` to `pager` with `count` bytes of its
/// object from `offset`, in `copy`, with the `control` port's send right,
/// whether they are `dirty` and whether it is a `kernel_copy`, as
/// `memory_object_data_return` sends them.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn data_return(
    pager: Reference<Port>,
    control: Reference<Port>,
    offset: usize,
    copy: CacheBox<MapCopy>,
    count: usize,
    dirty: bool,
    kernel_copy: bool,
) {
    let mut message = KernelMessage::allocate_saved()
        .unwrap_or_else(|_| panic!("memory_object_data_return: no buffer"));
    let (dirty_word, dirty) = boolean(dirty);
    let (kernel_copy_word, kernel_copy) = boolean(kernel_copy);
    // SAFETY: `DataReturn` is `repr(C)`, a `Header`, then integers and
    // ports' addresses only.
    let request = unsafe { message.layout_mut::<DataReturn>() };
    *request = DataReturn {
        header: header(
            MessageType::PortSend,
            pager,
            size_of::<DataReturn>(),
            MEMORY_OBJECT_DATA_RETURN,
        ),
        control_word: type_word(MessageType::PortSend, 64, 1),
        control: Reference::into_raw(control).as_ptr().expose_provenance(),
        offset_word: type_word(MessageType::Integer64, 64, 1),
        offset,
        data_word: out_of_line_word(MessageType::Byte, 8, u32::try_from(count).unwrap()),
        data: CacheBox::into_raw(copy).as_ptr().expose_provenance(),
        dirty_word,
        dirty,
        kernel_copy_word,
        kernel_copy,
    };
    mach_msg::send::send_always(message);
}

/// Sends `memory_object_change_completed` to `notification`, a send-once
/// right: whether its object may cache and how it copies.
///
/// # Panics
///
/// If no buffer can be allocated.
pub fn change_completed(notification: Reference<Port>, may_cache: bool, strategy: i32) {
    let mut message = KernelMessage::allocate_saved()
        .unwrap_or_else(|_| panic!("memory_object_change_completed: no buffer"));
    let (may_cache_word, may_cache) = boolean(may_cache);
    let (strategy_word, strategy) = integer32(strategy);
    // SAFETY: `ChangeCompleted` is `repr(C)`, a `Header`, then integers and
    // ports' addresses only.
    let request = unsafe { message.layout_mut::<ChangeCompleted>() };
    *request = ChangeCompleted {
        // No rights in its body: not complex.
        header: Header {
            bits: MessageBits::new(Some(MessageType::PortSendOnce), None),
            size: u32::try_from(size_of::<ChangeCompleted>()).unwrap(),
            remote_port: Reference::into_raw(notification)
                .as_ptr()
                .expose_provenance(),
            local_port: 0,
            sequence_number: 0,
            id: MEMORY_OBJECT_CHANGE_COMPLETED,
        },
        may_cache_word,
        may_cache,
        strategy_word,
        strategy,
    };
    mach_msg::send::send_always(message);
}

/// A copy of `page`'s data, `PAGE_SIZE` bytes of the kernel's IPC map, as
/// `memory_object_data_return` carries it, and its count.
///
/// # Panics
///
/// If the IPC map can't be allocated from or made pageable: not yet. As
/// `vm::user::allocate` and `Map::pageable`.
pub(in crate::vm) fn page_copy(page: &'static Page) -> (CacheBox<MapCopy>, usize) {
    let map = crate::ipc::KERNEL_MAP
        .get()
        .expect("memory_object_data_return: no ipc_kernel_map");
    let size = PAGE_SIZE;
    let start = user::allocate(map, Placement::Anywhere, size).unwrap_or_else(|error| {
        panic!("memory_object_data_return: vm_allocate: {error:?}, not yet")
    });
    let range = start..start + size;
    if let Err(error) = map.pageable(
        range.clone(),
        Protection::READ | Protection::WRITE,
        RangeCheck::Checked,
    ) {
        panic!("memory_object_data_return: vm_map_pageable: {error:?}");
    }
    // SAFETY: the allocation is `size` bytes of the IPC map, page-aligned
    // and wired here; nothing else reads or writes it.
    unsafe {
        ptr::copy_nonoverlapping(
            page.address().direct_map_ptr::<u8>(),
            ptr::with_exposed_provenance_mut::<u8>(usize::from(start)),
            size,
        );
    }
    if let Err(error) = map.pageable(range.clone(), Protection::empty(), RangeCheck::Checked) {
        panic!("memory_object_data_return: vm_map_pageable: {error:?}");
    }
    (map.copy_in(range, Source::Destroy), size)
}

/// Marks the pages of `object` from `offset`, `size` bytes, as having no
/// data, as `memory_object_data_unavailable` does: those a fault waits for
/// stop being busy, and the fault fills them with zeros.
///
/// # Errors
///
/// `InvalidArgument` if `size` is not a whole number of pages.
///
/// # Panics
///
/// As `Page::activate`.
pub fn data_unavailable(
    object: &Reference<Object>,
    offset: usize,
    size: usize,
) -> Result<(), Error> {
    if !size.is_multiple_of(PAGE_SIZE) {
        return Err(Error::InvalidArgument);
    }
    let mut state = object.lock();
    state.begin_paging();
    for page_offset in (offset..offset + size).step_by(PAGE_SIZE) {
        let Some(page) = state
            .lookup(page_offset)
            .filter(|page| page.is_absent() && page.is_busy())
        else {
            continue;
        };
        page.clear_busy();
        page.activate();
    }
    state.end_paging();
    drop(state);
    // The faults waiting for those pages go on.
    wait::wakeup(Event::of(&**object));
    Ok(())
}

/// Marks the pages of `object` from `offset`, `size` bytes, as in error, as
/// `memory_object_data_error` does: those a fault waits for are filled
/// nowhere, and the fault fails.
///
/// # Errors
///
/// `InvalidArgument` if `size` is not a whole number of pages.
///
/// # Panics
///
/// As `Page::activate`.
pub fn data_error(object: &Reference<Object>, offset: usize, size: usize) -> Result<(), Error> {
    if !size.is_multiple_of(PAGE_SIZE) {
        return Err(Error::InvalidArgument);
    }
    let mut state = object.lock();
    state.begin_paging();
    for page_offset in (offset..offset + size).step_by(PAGE_SIZE) {
        let Some(page) = state
            .lookup(page_offset)
            .filter(|page| page.is_absent() && page.is_busy())
        else {
            continue;
        };
        page.set_error();
        page.clear_absent();
        page.clear_busy();
        page.activate();
    }
    state.end_paging();
    drop(state);
    // The faults waiting for those pages go on, to fail.
    wait::wakeup(Event::of(&**object));
    Ok(())
}

/// Fills the pages of `object` from `offset` with `data`, `count` bytes, as
/// `memory_object_data_supply` does, holding them from `lock_value`'s
/// accesses and marking them `precious`; tells `reply_to` how it went.
///
/// # Errors
///
/// `InvalidArgument` if `lock_value` names accesses Mach lacks, or the data
/// is empty or not a whole number of pages.
///
/// # Panics
///
/// If a page is busy with another supply, or a page or its data can't be
/// got: not yet. As `kalloc::allocate`, `MapCopy::read_pages`,
/// `page::alloc::allocate` and `Page::activate`.
pub fn data_supply(
    object: &Reference<Object>,
    offset: usize,
    data: &CacheBox<MapCopy>,
    count: usize,
    lock_value: u32,
    precious: bool,
    reply_to: Option<Reference<Port>>,
) -> Result<(), Error> {
    if lock_value & !Protection::all().bits() != 0 || count == 0 || !count.is_multiple_of(PAGE_SIZE)
    {
        return Err(Error::InvalidArgument);
    }
    // The data may be the sender's map's: copied where it can be read.
    let buffer = kalloc::allocate(count)
        .unwrap_or_else(|error| panic!("memory_object_data_supply: kalloc: {error:?}, not yet"));
    // SAFETY: `count` bytes, ours alone, untouched before the read fills
    // them.
    let bytes = unsafe { core::slice::from_raw_parts_mut(buffer.as_ptr(), count) };
    data.read_pages(0, bytes);
    let page_lock = Protection::from_bits_retain(lock_value);
    let mut result = 0;
    let mut error_offset = 0;
    let mut state = object.lock();
    state.begin_paging();
    for page in 0..count / PAGE_SIZE {
        let page_offset = offset + page * PAGE_SIZE;
        // A page whose data a fault waits for leaves; one that is in
        // already is what GNU Mach reports.
        let was_absent = match state.lookup(page_offset) {
            Some(page) if page.is_absent() && page.is_busy() => {
                state.remove_pages(page_offset..page_offset + PAGE_SIZE);
                true
            }
            Some(page) if !page.is_busy() => {
                // KERN_MEMORY_PRESENT.
                result = 23;
                error_offset = page_offset;
                break;
            }
            Some(_) => panic!("memory_object_data_supply: a busy page, not yet"),
            None => false,
        };
        let new = page::alloc::allocate(page_offset / PAGE_SIZE)
            .expect("memory_object_data_supply: VM_FAULT_MEMORY_SHORTAGE, not yet");
        // SAFETY: the page is in no object and nothing else uses it.
        let destination = unsafe {
            core::slice::from_raw_parts_mut(new.address().direct_map_ptr::<u8>(), PAGE_SIZE)
        };
        destination.copy_from_slice(&bytes[page * PAGE_SIZE..][..PAGE_SIZE]);
        new.set_page_lock(page_lock);
        if precious {
            new.set_precious();
        }
        state.insert(object, new, page_offset);
        if was_absent {
            new.activate();
        } else {
            new.deactivate();
        }
    }
    state.end_paging();
    let request = state.pager_request.as_ref().map(Port::make_send);
    drop(state);
    // SAFETY: from `kalloc::allocate`, `count` bytes, not used again.
    unsafe { kalloc::free(buffer, count) };
    // The faults waiting for those pages go on.
    wait::wakeup(Event::of(&**object));
    if let Some(reply_to) = reply_to {
        let request = request.expect("memory_object_data_supply: no request port");
        supply_completed(reply_to, request, offset, count, result, error_offset);
    }
    Ok(())
}

/// Holds the pages of `object` from `offset`, `size` bytes, from
/// `lock_value`'s accesses, gives those among them `should_return` wants
/// back to its pager, drops those `should_flush` says, and tells
/// `reply_to` that it is done, as `memory_object_lock_request` does.
///
/// # Errors
///
/// `InvalidArgument` if `lock_value` names accesses Mach lacks.
///
/// # Panics
///
/// If a page is busy, more than 32 pages are to be returned, or a page's
/// data can't be got: not yet. As `page_copy`.
pub fn lock_request(
    object: &Reference<Object>,
    offset: usize,
    size: usize,
    should_return: i32,
    should_flush: bool,
    lock_value: i32,
    reply_to: Option<Reference<Port>>,
) -> Result<(), Error> {
    // VM_PROT_NO_CHANGE, which leaves the lock as it is.
    const NO_CHANGE: i32 = 0x08;
    if lock_value & !i32::try_from(Protection::all().bits()).unwrap() != 0
        && lock_value != NO_CHANGE
    {
        return Err(Error::InvalidArgument);
    }
    let original = (offset, size);
    let size = size.next_multiple_of(PAGE_SIZE);
    let page_lock = if lock_value == NO_CHANGE {
        None
    } else {
        Some(Protection::from_bits_retain(lock_value.cast_unsigned()))
    };
    // The pages' data to give back, at most 32 at a time, as GNU Mach does.
    let mut returns = [const { (0, None) }; 32];
    let mut returned = 0;
    let mut state = object.lock();
    state.begin_paging();
    for page_offset in (offset..offset + size).step_by(PAGE_SIZE) {
        let Some(page) = state.lookup(page_offset) else {
            continue;
        };
        // No data to hold or give back.
        if page.is_absent() {
            continue;
        }
        assert!(
            !page.is_busy(),
            "memory_object_lock_request: a busy page, not yet"
        );
        if let Some(page_lock) = page_lock {
            page.set_page_lock(page_lock);
            // Past unlock requests are over.
            page.set_unlock_request(Protection::empty());
        }
        // As memory_object_lock_page: dirty data, or precious data when
        // all is asked for, goes back.
        if should_return != 0 && (page.is_dirty() || (page.is_precious() && should_return == 2)) {
            assert!(
                returned != returns.len(),
                "memory_object_lock_request: more than 32 pages, not yet"
            );
            page.clear_dirty();
            returns[returned] = (page_offset, Some(page_copy(page)));
            returned += 1;
        }
    }
    state.end_paging();
    let request = state.pager_request.as_ref().map(Port::make_send);
    drop(state);
    // The faults held off by the locks, and those whose unlock was asked
    // for, go on.
    wait::wakeup(Event::of(&**object));
    for (page_offset, copy) in returns.into_iter().take(returned) {
        let Some((copy, count)) = copy else {
            continue;
        };
        let request = request
            .as_ref()
            .map(Port::make_send)
            .expect("memory_object_lock_request: no request port");
        let pager = object
            .pager_port()
            .expect("memory_object_lock_request: no pager");
        data_return(
            pager,
            request,
            page_offset,
            copy,
            count,
            true,
            !should_flush,
        );
    }
    if let Some(reply_to) = reply_to {
        let request = request.expect("memory_object_lock_request: no request port");
        lock_completed(reply_to, request, original.0, original.1);
    }
    Ok(())
}

/// Sets what `object` may cache and how it copies, as
/// `memory_object_change_attributes` does, and tells `reply_to`.
///
/// # Panics
///
/// As `change_completed`.
pub fn change_attributes(
    object: &Reference<Object>,
    may_cache: bool,
    copy_strategy: CopyStrategy,
    reply_to: Option<Reference<Port>>,
) {
    let mut state = object.lock();
    // As memory_object_set_attributes_common.
    if may_cache {
        state.flags.insert(Flags::CAN_PERSIST);
    } else {
        state.flags.remove(Flags::CAN_PERSIST);
    }
    state.copy_strategy = copy_strategy;
    drop(state);
    if let Some(reply_to) = reply_to {
        change_completed(reply_to, may_cache, copy_strategy.number());
    }
}
