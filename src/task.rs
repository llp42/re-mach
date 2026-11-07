// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Tasks: an address space, a name space for ports, and threads.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicU32, AtomicUsize, Ordering};
use core::{iter, str};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::arch::{self, MAX_USER_ADDRESS, MIN_USER_ADDRESS, Machine as _};
use crate::ast::Ast;
use crate::clock::{self, AtomicTimeValue};
use crate::error::Error;
use crate::ipc::port::{self, KernelObject, Port};
use crate::ipc::space::Space;
use crate::processor::{DEFAULT_SET, ProcessorSet};
use crate::sched::{RUN_QUEUES, USER_PRIORITY};
use crate::slab::{Cache, CacheBox};
use crate::smp;
use crate::sync::{CriticalSection, OnceLock, Token};
use crate::thread::{TaskThreadAdapter, Thread};
use crate::vm::kernel::KERNEL_MAP;
use crate::vm::map::Map;
use crate::vm::pmap::MachinePmap as _;

/// The bytes a task's name holds, its final NUL included.
pub const NAME_SIZE: usize = 32;

/// A task; so far only what a new task sets.
#[expect(dead_code, reason = "the task calls read them")]
pub struct Task {
    /// How many references it has.
    references: AtomicU32,
    /// Its address space.
    pub(super) map: UnsafeRef<Map>,
    /// Its names for ports and port sets.
    pub(super) space: UnsafeRef<Space>,
    /// Its port, which controls it.
    pub(super) self_port: AtomicPtr<Port>,
    /// The send right for its port it hands out.
    self_send: AtomicPtr<Port>,
    /// Whether it may be moved to another processor set.
    may_assign: bool,
    /// When it was created.
    pub(super) creation_time: AtomicTimeValue,
    /// Its page faults: each search for a faulting page counts one.
    pub(super) faults: AtomicUsize,
    /// Its pages filled with zeros on a fault.
    pub(super) zero_fills: AtomicUsize,
    /// Its name, ending with a NUL.
    pub(super) name: [AtomicU8; NAME_SIZE],
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
pub struct TaskMembers {
    /// Whether the task is alive.
    pub(super) active: bool,
    /// The priority of the task's threads.
    pub(super) priority: u32,
    /// The highest priority the task's threads may take.
    pub(super) max_priority: u32,
    /// How many times the task is suspended.
    pub(super) suspend_count: u32,
    /// How many times users stopped the task; the first suspends it.
    user_stop_count: u32,
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

    /// Stops it once more for users: the first stop suspends it, holding its
    /// threads and waiting for them to stop; the current thread, if one of
    /// them, is held and blocks on its way out of the kernel.
    ///
    /// # Panics
    ///
    /// If it is not alive, or has a thread other than the current one to
    /// wait for: not yet.
    pub(super) fn suspend(&self) {
        let mut members = self.members.lock();
        members.user_stop_count += 1;
        let first = members.user_stop_count == 1;
        drop(members);
        if !first {
            return;
        }
        self.hold();
        self.wait_until_stopped();
        let current = smp::current()
            .active_thread()
            .expect("task_suspend: no current thread");
        if ptr::eq(&raw const *current.task, self) {
            current.hold();
            let _section = CriticalSection::enter();
            smp::current().add_asts(Ast::BLOCK);
        }
    }

    /// Suspends it once more, holding its threads but the current one.
    ///
    /// # Panics
    ///
    /// If it is not alive: not yet.
    fn hold(&self) {
        let current = smp::current()
            .active_thread()
            .expect("task_hold: no current thread");
        let mut members = self.members.lock();
        assert!(members.active, "task_hold: an inactive task, not yet");
        members.suspend_count += 1;
        for thread in &members.threads {
            if !ptr::eq(thread, current) {
                thread.hold();
            }
        }
        drop(members);
    }

    /// Waits until its threads but the current one stop.
    ///
    /// # Panics
    ///
    /// If it is not alive, or has a thread other than the current one: not
    /// yet.
    fn wait_until_stopped(&self) {
        let current = smp::current()
            .active_thread()
            .expect("task_dowait: no current thread");
        let members = self.members.lock();
        for thread in &members.threads {
            assert!(members.active, "task_dowait: an inactive task, not yet");
            assert!(
                ptr::eq(thread, current),
                "task_dowait: thread_dowait, not yet"
            );
        }
        drop(members);
    }

    /// Names it `name`, cut to fit with its final NUL.
    pub(super) fn set_name(&self, name: &str) {
        // Unlocked, as in GNU Mach: racing calls may mix two names.
        let bytes = name.bytes().chain(iter::repeat(0));
        for (stored, byte) in self.name[..NAME_SIZE - 1].iter().zip(bytes) {
            stored.store(byte, Ordering::Relaxed);
        }
    }

    /// Caps its priority at `max_priority`, as the boot script does with
    /// `host`, the host's port: its priority set to the cap, and its
    /// threads' too.
    ///
    /// # Panics
    ///
    /// If `host` is not a host's port or `max_priority` is not a priority,
    /// if the cap is raised, or if it has threads: not yet.
    pub(super) fn set_max_priority(&self, host: &Port, max_priority: u32) {
        assert!(
            matches!(
                host.kernel_object(),
                Some(KernelObject::Host(_) | KernelObject::HostPrivileged(_))
            ) && max_priority < u32::try_from(RUN_QUEUES).unwrap(),
            "task_max_priority: an invalid argument, not yet"
        );
        let mut members = self.members.lock();
        // A larger number is lower.
        assert!(
            max_priority >= members.max_priority,
            "task_max_priority: a raised cap, not yet"
        );
        members.max_priority = max_priority;
        members.priority = max_priority;
        assert!(
            members.threads.is_empty(),
            "thread_override_max_priority: not yet"
        );
        drop(members);
    }
}

/// The kernel's task, once `init` creates it.
pub static KERNEL_TASK: OnceLock<&'static Task> = OnceLock::new();

/// The cache tasks come from.
static CACHE: Cache<Task> = Cache::new("task");

/// Sets up tasks and creates the kernel's, over the kernel map, on the boot
/// CPU.
///
/// # Panics
///
/// If the kernel task can't be created, or on a second call.
pub fn init() {
    CACHE.register();
    arch::Current::task_module_init();
    let map = KERNEL_MAP.get().expect("task_init: no kernel map");
    let task = create_with_map(map).unwrap_or_else(|error| panic!("task_init: {error:?}"));
    task.set_name("gnumach");
    assert!(KERNEL_TASK.set(task).is_ok(), "task_init: twice");
}

/// A new task with no parent, over a new map of user space and its own
/// physical map, made as `create_with_map` makes it.
///
/// # Errors
///
/// `ResourceShortage` if the physical map can't be allocated.
///
/// # Panics
///
/// With a parent: not yet. If the map, the space, the port or the task
/// can't be allocated: freeing what was taken, not yet.
pub fn create(parent: Option<&Task>) -> Result<&'static Task, Error> {
    assert!(parent.is_none(), "task_create_kernel: a parent, not yet");
    let pmap = arch::Pmap::create()?;
    let map = Map::create(pmap, MIN_USER_ADDRESS..MAX_USER_ADDRESS)
        .unwrap_or_else(|error| panic!("task_create_kernel: {error:?}, pmap_destroy not yet"));
    // SAFETY: a task's map lives until vm_map_deallocate frees it, which
    // comes later.
    let map = unsafe { CacheBox::into_raw(map).as_ref() };
    Ok(create_with_map(map)
        .unwrap_or_else(|error| panic!("task_create_kernel: {error:?}, vm_map_deallocate not yet")))
}

/// A new task with no parent over `map`, named by its address: a new
/// space, and a port with a send right, at the default set's priorities,
/// in the default set.
fn create_with_map(map: &'static Map) -> Result<&'static Task, Error> {
    let mut space = Space::create()?;
    let port = port::allocate_kernel()?;
    port.make_send();
    let task = CACHE.allocate(Task {
        references: AtomicU32::new(2),
        // SAFETY: a task's map outlives it.
        map: unsafe { UnsafeRef::from_raw(ptr::from_ref(map)) },
        // SAFETY: its box, unused until given up below, outlives the task.
        space: unsafe { UnsafeRef::from_raw(ptr::from_mut(&mut *space)) },
        self_port: AtomicPtr::new(ptr::null_mut()),
        self_send: AtomicPtr::new(ptr::null_mut()),
        may_assign: true,
        creation_time: clock::stamp().into(),
        faults: AtomicUsize::new(0),
        zero_fills: AtomicUsize::new(0),
        name: [const { AtomicU8::new(0) }; NAME_SIZE],
        members: Token::new(TaskMembers {
            active: true,
            // No higher than the set allows; a larger number is lower.
            priority: USER_PRIORITY.max(DEFAULT_SET.max_priority),
            max_priority: DEFAULT_SET.max_priority,
            suspend_count: 0,
            user_stop_count: 0,
            threads: LinkedList::new(TaskThreadAdapter::NEW),
            thread_count: 0,
        }),
        set: AtomicPtr::new(ptr::null_mut()),
        set_link: LinkedListAtomicLink::new(),
    })?;
    // The task takes the space's first reference.
    CacheBox::into_raw(space);
    let task = CacheBox::into_raw(task);
    // SAFETY: a task lives until task_deallocate frees it, which comes
    // later.
    let task = unsafe { task.as_ref() };
    // "%p": its address in hex, with no prefix, written from the end.
    let mut name = [0; 2 * size_of::<usize>()];
    let (mut start, mut address) = (name.len(), ptr::from_ref(task).addr());
    loop {
        start -= 1;
        name[start] = b"0123456789abcdef"[address % 16];
        address /= 16;
        if address == 0 {
            break;
        }
    }
    task.set_name(str::from_utf8(&name[start..]).unwrap());
    port.set_kernel_object(KernelObject::Task(NonNull::from(task)));
    let port = CacheBox::into_raw(port).as_ptr();
    task.self_port.store(port, Ordering::Release);
    task.self_send.store(port, Ordering::Release);
    DEFAULT_SET.add_task(task);
    Ok(task)
}
