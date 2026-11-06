// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Tasks: an address space, a name space for ports, and threads.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use super::Error;
use super::clock::{self, AtomicTimeValue};
use super::processor::{DEFAULT_SET, ProcessorSet};
use super::sched::USER_PRIORITY;
use super::slab::{Cache, CacheBox};
use super::thread::{TaskThreadAdapter, Thread};
use crate::arch::{self, Machine as _};
use crate::ipc::port::{self, KernelObject, Port};
use crate::ipc::space::Space;
use crate::sync::OnceLock;
use crate::sync::Token;
use crate::vm::kernel::KERNEL_MAP;
use crate::vm::map::Map;

/// The bytes a task's name holds, its final NUL included.
pub const NAME_SIZE: usize = 32;

/// A task; so far only what a new task sets.
#[expect(dead_code, reason = "the task calls read them")]
pub struct Task {
    /// How many references it has.
    references: AtomicU32,
    /// Its address space.
    map: UnsafeRef<Map>,
    /// Its names for ports and port sets.
    space: UnsafeRef<Space>,
    /// Its port, which controls it.
    pub(super) self_port: AtomicPtr<Port>,
    /// The send right for its port it hands out.
    self_send: AtomicPtr<Port>,
    /// Whether it may be moved to another processor set.
    may_assign: bool,
    /// When it was created.
    pub(super) creation_time: AtomicTimeValue,
    /// Its name, ending with a NUL.
    pub(super) name: [u8; NAME_SIZE],
    /// Whether it is alive, its priorities and suspensions, and its threads.
    pub(super) members: Token<TaskMembers>,
    /// The processor set its threads run on; set as the set takes it.
    pub(super) set: AtomicPtr<ProcessorSet>,
    /// Its place in its set's tasks.
    set_link: LinkedListAtomicLink,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub SetTaskAdapter = UnsafeRef<Task>: Task { set_link => LinkedListAtomicLink }
);

/// Whether a task is alive, its priorities and suspensions, and its threads.
pub(super) struct TaskMembers {
    /// Whether the task is alive.
    pub(super) active: bool,
    /// The priority of the task's threads.
    pub(super) priority: u32,
    /// The highest priority the task's threads may take.
    pub(super) max_priority: u32,
    /// How many times the task is suspended.
    pub(super) suspend_count: u32,
    /// The threads, in the order the task took them.
    threads: LinkedList<TaskThreadAdapter>,
    /// How many threads there are.
    thread_count: u32,
}

impl Task {
    /// Takes `thread`, new, into its `members`, with a reference for it.
    pub(super) fn add_thread(&self, members: &mut TaskMembers, thread: &'static Thread) {
        self.references.fetch_add(1, Ordering::Relaxed);
        members.thread_count += 1;
        // SAFETY: a thread leaves its task before it is freed.
        members
            .threads
            .push_back(unsafe { UnsafeRef::from_raw(thread) });
    }
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
    let mut space = Space::create()?;
    let mut port = port::allocate_kernel()?;
    port.make_send();
    let mut name = [0; NAME_SIZE];
    name[..7].copy_from_slice(b"gnumach");
    let task = CACHE.allocate(Task {
        references: AtomicU32::new(2),
        // SAFETY: the kernel map is static.
        map: unsafe { UnsafeRef::from_raw(ptr::from_ref(map)) },
        // SAFETY: its box, unused until given up below, outlives the task.
        space: unsafe { UnsafeRef::from_raw(ptr::from_mut(&mut *space)) },
        self_port: AtomicPtr::new(ptr::null_mut()),
        self_send: AtomicPtr::new(ptr::null_mut()),
        may_assign: true,
        creation_time: clock::stamp().into(),
        name,
        members: Token::new(TaskMembers {
            active: true,
            // No higher than the set allows; a larger number is lower.
            priority: USER_PRIORITY.max(DEFAULT_SET.max_priority),
            max_priority: DEFAULT_SET.max_priority,
            suspend_count: 0,
            threads: LinkedList::new(TaskThreadAdapter::NEW),
            thread_count: 0,
        }),
        set: AtomicPtr::new(ptr::null_mut()),
        set_link: LinkedListAtomicLink::new(),
    })?;
    // The task takes the space's first reference.
    CacheBox::into_raw(space);
    let task = CacheBox::into_raw(task);
    // SAFETY: the task is the kernel's for good.
    let task = unsafe { task.as_ref() };
    port.set_kernel_object(KernelObject::Task(NonNull::from(task)));
    let port = CacheBox::into_raw(port).as_ptr();
    task.self_port.store(port, Ordering::Release);
    task.self_send.store(port, Ordering::Release);
    DEFAULT_SET.add_task(task);
    Ok(task)
}
