// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Tasks: an address space, a name space for ports, and threads.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use intrusive_collections::UnsafeRef;

use super::Error;
use super::clock::{self, TimeValue};
use super::processor::DEFAULT_SET;
use super::sched::USER_PRIORITY;
use super::slab::{Cache, CacheBox};
use crate::arch::{self, Machine as _};
use crate::ipc::port::{self, KernelObject, Port};
use crate::ipc::space::Space;
use crate::sync::OnceLock;
use crate::vm::kernel::KERNEL_MAP;
use crate::vm::map::Map;

/// The bytes a task's name holds, its final NUL included.
const NAME_SIZE: usize = 32;

/// A task; so far only what a new task sets.
#[expect(dead_code, reason = "the task calls read them")]
pub struct Task {
    /// How many references it has.
    references: AtomicU32,
    /// Whether it is alive.
    active: bool,
    /// Its address space.
    map: UnsafeRef<Map>,
    /// Its names for ports and port sets.
    space: UnsafeRef<Space>,
    /// Its port, which controls it.
    self_port: AtomicPtr<Port>,
    /// The send right for its port it hands out.
    self_send: AtomicPtr<Port>,
    /// Its threads' priority.
    priority: u32,
    /// The highest priority its threads may take.
    max_priority: u32,
    /// Whether it may be moved to another processor set.
    may_assign: bool,
    /// When it was created.
    creation_time: TimeValue,
    /// Its name, ending with a NUL.
    name: [u8; NAME_SIZE],
}

/// The kernel's task, once `init` creates it.
pub static KERNEL_TASK: OnceLock<&'static Task> = OnceLock::new();

/// The cache tasks come from.
static CACHE: Cache<Task> = Cache::new("task");

/// Sets up tasks and creates the kernel's, on the boot CPU.
///
/// # Panics
///
/// If the kernel task can't be created, or on a second call.
pub fn init() {
    CACHE.register();
    arch::Current::task_module_init();
    let task = create_kernel().unwrap_or_else(|error| panic!("task_init: {error:?}"));
    assert!(KERNEL_TASK.set(task).is_ok(), "task_init: twice");
}

/// The kernel's task, named "gnumach": the kernel map, a new space, and a
/// port with a send right, at the default set's priorities.
fn create_kernel() -> Result<&'static Task, Error> {
    let map = KERNEL_MAP.get().expect("task_init: no kernel map");
    let space = Space::create()?;
    let mut port = port::allocate_kernel()?;
    port.make_send();
    let mut name = [0; NAME_SIZE];
    name[..7].copy_from_slice(b"gnumach");
    let task = CACHE.allocate(Task {
        references: AtomicU32::new(2),
        active: true,
        // SAFETY: the kernel map is static.
        map: unsafe { UnsafeRef::from_raw(ptr::from_ref(map)) },
        // SAFETY: the task takes the space's first reference.
        space: unsafe { UnsafeRef::from_raw(CacheBox::into_raw(space).as_ptr()) },
        self_port: AtomicPtr::new(ptr::null_mut()),
        self_send: AtomicPtr::new(ptr::null_mut()),
        // No higher than the set allows; a larger number is lower.
        priority: USER_PRIORITY.max(DEFAULT_SET.max_priority),
        max_priority: DEFAULT_SET.max_priority,
        may_assign: true,
        creation_time: clock::stamp(),
        name,
    })?;
    let task = CacheBox::into_raw(task);
    // SAFETY: the task is the kernel's for good.
    let task = unsafe { task.as_ref() };
    port.set_kernel_object(KernelObject::Task(NonNull::from(task)));
    let port = CacheBox::into_raw(port).as_ptr();
    task.self_port.store(port, Ordering::Release);
    task.self_send.store(port, Ordering::Release);
    Ok(task)
}
