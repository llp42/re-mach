// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::ManuallyDrop;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink};

use crate::arch::{self, MAX_USER_ADDRESS, MIN_USER_ADDRESS, Machine as _};
use crate::clock;
use crate::error::Error;
use crate::ipc::port::{KernelObject, Port, SendRight};
use crate::ipc::space::Space;
use crate::name::Name;
use crate::processor::DEFAULT_SET;
use crate::reference::{Count, Reference};
use crate::sched::USER_PRIORITY;
use crate::slab::{Cache, CacheBox};
use crate::sync::{OnceLock, Spinlock, Token};
use crate::thread::thread::TaskThreadAdapter;
use crate::vm::VirtualAddress;
use crate::vm::kernel::KERNEL_MAP;
use crate::vm::map::Map;
use crate::vm::phys_map::MachinePhysMap as _;

use super::members::TaskMembers;
use super::ports::{Memory, TaskPorts};
use super::task::Task;

/// The kernel's task, once `init` creates it.
pub static KERNEL_TASK: OnceLock<&'static Task> = OnceLock::new();

/// The cache tasks come from.
pub(super) static CACHE: Cache<Task> = Cache::new("task");

impl Task {
    /// A new task, as `task_create_kernel` makes it: over a copy of
    /// `parent`'s map for `Memory::Inherited`, else over a new map of user
    /// space and its own physical map, made as `create_with_map` makes it.
    /// Gives the caller's reference.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the physical map can't be allocated.
    ///
    /// # Panics
    ///
    /// For `Memory::Inherited` with no parent. If the map, the space, the
    /// port or the task can't be allocated: freeing what was taken, not yet.
    /// If a port it inherits is dead: not yet. As `Map::fork`.
    pub fn create(parent: Option<&Self>, memory: Memory) -> Result<Reference<Self>, Error> {
        let map = match memory {
            Memory::Inherited => parent
                .expect("task_create_kernel: inherit_memory with no parent")
                .map
                .fork()?,
            Memory::Empty => {
                let phys_map = arch::PhysMap::create()?;
                let mut map = Map::create(
                    phys_map,
                    VirtualAddress::from(MIN_USER_ADDRESS)..VirtualAddress::from(MAX_USER_ADDRESS),
                )
                .unwrap_or_else(|error| {
                    panic!("task_create_kernel: {error:?}, pmap_destroy not yet")
                });
                if let Some(parent) = parent {
                    map.copy_limits(&parent.map);
                }
                map
            }
        };
        let map = CacheBox::into_raw(map);
        // The box's provenance, which the map's free takes back from a handle
        // made from a shared borrow.
        map.expose_provenance();
        // SAFETY: the box, given up, holds the map's first reference, which
        // the task takes.
        let map = unsafe { map.as_ref() };
        let task = create_with_map(map, parent).unwrap_or_else(|error| {
            panic!("task_create_kernel: {error:?}, vm_map_deallocate not yet")
        });
        // SAFETY: the caller's reference, which the handle takes.
        Ok(unsafe { Reference::from_raw(NonNull::from(task)) })
    }
}

/// Sets up tasks and creates the kernel's, over the kernel map, on the boot
/// CPU.
///
/// # Panics
///
/// If the kernel task can't be created, or on a second call.
pub fn init() {
    CACHE.register();
    arch::Current::task_module_init();
    // The kernel task takes the kernel map's one reference.
    let map = KERNEL_MAP.get().expect("task_init: no kernel map");
    let task = create_with_map(map, None).unwrap_or_else(|error| panic!("task_init: {error:?}"));
    task.members.lock().name.set(format_args!("gnumach"));
    assert!(KERNEL_TASK.set(task).is_ok(), "task_init: twice");
}

/// A new task over `map`, whose reference the caller gives, as
/// `task_create_kernel` makes it: a new space, and a port with a send right;
/// with `parent`, copies of its registered, exception and bootstrap ports,
/// its set, its priorities and its name in parentheses; else in the default
/// set, at its priorities, named by its address.
fn create_with_map(map: &'static Map, parent: Option<&Task>) -> Result<&'static Task, Error> {
    let mut space = Space::create()?;
    let port = Port::allocate_kernel()?;
    // As ipc_task_init: the parent's registered, exception and bootstrap
    // ports.
    let copy = |slot: &Option<SendRight>| {
        slot.as_ref()
            .map(|port| SendRight::new(Port::copy_send(port.as_ref())))
    };
    let inherited = parent.map(|parent| parent.ports.lock());
    let registered = inherited.as_ref().map_or([const { None }; 4], |ports| {
        ports.registered.each_ref().map(copy)
    });
    let exception = inherited.as_ref().and_then(|ports| copy(&ports.exception));
    let bootstrap = inherited.as_ref().and_then(|ports| copy(&ports.bootstrap));
    drop(inherited);
    let mut task = CACHE.allocate(Task {
        // One for being alive, one for the caller.
        references: Count::new(2),
        // SAFETY: the caller's reference to the map, which the task takes.
        map: unsafe { Reference::from_raw(NonNull::from(map)) },
        // SAFETY: the box, given up below, holds the task's reference.
        space: unsafe { Reference::from_raw(NonNull::from(&mut *space)) },
        ports: Spinlock::new(TaskPorts {
            self_port: None,
            self_send: None,
            exception,
            bootstrap,
            registered,
        }),
        may_assign: true,
        creation_time: clock::time::stamp().into(),
        total_user_time: AtomicU64::new(0),
        total_system_time: AtomicU64::new(0),
        faults: AtomicUsize::new(0),
        zero_fills: AtomicUsize::new(0),
        pageins: AtomicUsize::new(0),
        cow_faults: AtomicUsize::new(0),
        messages_sent: AtomicUsize::new(0),
        messages_received: AtomicUsize::new(0),
        members: Token::new(TaskMembers {
            active: true,
            // The default set's, no higher than it allows; a parent's below.
            priority: USER_PRIORITY.weaker(DEFAULT_SET.max_priority()),
            max_priority: DEFAULT_SET.max_priority(),
            suspend_count: 0,
            user_stop_count: 0,
            threads: LinkedList::new(TaskThreadAdapter::NEW),
            thread_count: 0,
            name: Name::new(),
        }),
        set: AtomicPtr::new(ptr::null_mut()),
        set_link: LinkedListAtomicLink::new(),
    })?;
    // The parent's set, or the default set if it is out of use, and the
    // parent's priorities, under its token; else the default set. The set
    // takes a reference.
    let set = parent.map_or_else(
        || {
            // SAFETY: the default set's own reference never drops; this
            // handle borrows it and never drops it.
            Reference::clone(&ManuallyDrop::new(unsafe {
                Reference::from_raw(NonNull::from(&DEFAULT_SET))
            }))
        },
        |parent| {
            let parent_members = parent.members.lock();
            // SAFETY: under the parent's token its reference keeps its set
            // alive, and the one taken here does after.
            let set = unsafe { &*parent.set.load(Ordering::Acquire) };
            let set = if set.is_active() { set } else { &DEFAULT_SET };
            // SAFETY: as above, a reference this handle borrows and never
            // drops.
            let set = Reference::clone(&ManuallyDrop::new(unsafe {
                Reference::from_raw(NonNull::from(set))
            }));
            let members = task.members.get_mut();
            members.priority = parent_members.priority;
            members.max_priority = parent_members.max_priority;
            drop(parent_members);
            set
        },
    );
    // Without its token, as the task is not published yet: "(%.*s)" of the
    // parent's name, or "%p", its address in hex, with no prefix.
    if let Some(parent) = parent {
        let parent_members = parent.members.lock();
        task.members
            .get_mut()
            .name
            .set_parenthesized(&parent_members.name);
        drop(parent_members);
    } else {
        let address = ptr::from_ref(&*task).addr();
        task.members.get_mut().name.set(format_args!("{address:x}"));
    }
    // The task takes the space's first reference.
    CacheBox::into_raw(space);
    let task = CacheBox::into_raw(task);
    // The box's provenance, which `free` takes back from a handle made from
    // a shared borrow.
    task.expose_provenance();
    // SAFETY: a task lives until task_deallocate frees it, which comes
    // later.
    let task = unsafe { task.as_ref() };
    let port = Reference::from(port);
    let send = Port::make_send(&port);
    port.set_kernel_object(KernelObject::Task(NonNull::from(task)));
    let mut ports = task.ports.lock();
    ports.self_port = Some(port);
    ports.self_send = Some(send);
    drop(ports);
    set.add_task(task);
    // The task keeps the set's reference.
    Reference::into_raw(set);
    Ok(task)
}
