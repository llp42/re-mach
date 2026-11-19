// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Tasks: an address space, a name space for ports, and threads.

use core::mem::{self, ManuallyDrop};
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::arch::{self, MAX_USER_ADDRESS, MIN_USER_ADDRESS, Machine as _};
use crate::ast::Ast;
use crate::clock;
use crate::clock::time::AtomicTimeValue;
use crate::error::Error;
use crate::ipc::port::{KernelObject, Port, SendRight};
use crate::ipc::space::Space;
use crate::name::Name;
use crate::processor::{DEFAULT_SET, ProcessorSet};
use crate::reference::{Count, Counted, Reference};
use crate::sched::{Priority, USER_PRIORITY};
use crate::slab::{Cache, CacheBox};
use crate::smp;
use crate::sync::{CriticalSection, OnceLock, Spinlock, Token};
use crate::thread::{TaskThreadAdapter, Thread};
use crate::vm::VirtualAddress;
use crate::vm::kernel::KERNEL_MAP;
use crate::vm::map::Map;
use crate::vm::phys_map::MachinePhysMap as _;

/// A task; so far only what a new task sets.
#[expect(dead_code, reason = "the task calls read them")]
pub struct Task {
    /// How many references it has.
    references: Count,
    /// Its address space.
    pub(super) map: Reference<Map>,
    /// Its names for ports and port sets.
    pub(super) space: Reference<Space>,
    /// Its port, and the send right it hands out for it.
    pub(super) ports: Spinlock<TaskPorts>,
    /// Whether it may be moved to another processor set.
    may_assign: bool,
    /// When it was created.
    pub(super) creation_time: AtomicTimeValue,
    /// The time its ended threads ran in user mode, in microseconds.
    pub(super) total_user_time: AtomicU64,
    /// The time its ended threads ran in the kernel, in microseconds.
    pub(super) total_system_time: AtomicU64,
    /// Its page faults: each search for a faulting page counts one.
    pub(super) faults: AtomicUsize,
    /// Its pages filled with zeros on a fault.
    pub(super) zero_fills: AtomicUsize,
    /// Its pages asked of a pager on a fault.
    pub(super) pageins: AtomicUsize,
    /// Its pages copied up from a shadow object on a write fault.
    pub(super) cow_faults: AtomicUsize,
    /// The messages it sent.
    pub(super) messages_sent: AtomicUsize,
    /// The messages it received.
    pub(super) messages_received: AtomicUsize,
    /// Whether it is alive, its priorities and suspensions, its threads and
    /// its name.
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

/// A task's ports, under its lock; so far its own and its exception port.
pub struct TaskPorts {
    /// Its port, which controls it.
    pub(super) self_port: Option<Reference<Port>>,
    /// The send right for its port it hands out.
    self_send: Option<Reference<Port>>,
    /// The send right its threads' exceptions go to, if any.
    exception: Option<SendRight>,
}

/// A task's special port, by Mach's `TASK_*_PORT` number.
#[derive(Clone, Copy)]
pub enum SpecialPort {
    /// `TASK_KERNEL_PORT`: the send right it hands out for its port.
    Kernel,
    /// `TASK_EXCEPTION_PORT`: its exception port.
    Exception,
    /// `TASK_BOOTSTRAP_PORT`: its bootstrap port.
    Bootstrap,
}

impl TryFrom<i32> for SpecialPort {
    type Error = i32;

    /// The special port Mach's number `number` names, if any.
    fn try_from(number: i32) -> Result<Self, i32> {
        match number {
            1 => Ok(Self::Kernel),
            3 => Ok(Self::Exception),
            4 => Ok(Self::Bootstrap),
            _ => Err(number),
        }
    }
}

/// Whether a task is alive, its priorities and suspensions, its threads and
/// its name.
pub struct TaskMembers {
    /// Whether the task is alive.
    pub(super) active: bool,
    /// The priority of the task's threads.
    pub(super) priority: Priority,
    /// The highest priority the task's threads may take.
    pub(super) max_priority: Priority,
    /// How many times the task is suspended.
    pub(super) suspend_count: u32,
    /// How many times users stopped the task; the first suspends it.
    user_stop_count: u32,
    /// The threads, in the order the task took them.
    threads: LinkedList<TaskThreadAdapter>,
    /// How many threads there are.
    thread_count: u32,
    /// The task's name, ending with a NUL.
    pub(super) name: Name,
}

impl Task {
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
    pub fn create(parent: Option<&Self>) -> Result<&'static Self, Error> {
        assert!(parent.is_none(), "task_create_kernel: a parent, not yet");
        let phys_map = arch::PhysMap::create()?;
        let map = Map::create(
            phys_map,
            VirtualAddress::from(MIN_USER_ADDRESS)..VirtualAddress::from(MAX_USER_ADDRESS),
        )
        .unwrap_or_else(|error| panic!("task_create_kernel: {error:?}, pmap_destroy not yet"));
        // SAFETY: the box, given up, holds the map's first reference, which
        // the task takes.
        let map = unsafe { CacheBox::into_raw(map).as_ref() };
        Ok(create_with_map(map).unwrap_or_else(|error| {
            panic!("task_create_kernel: {error:?}, vm_map_deallocate not yet")
        }))
    }

    /// Takes `thread`, new, into its `members`, with a reference for it.
    pub(super) fn add_thread(&self, members: &mut TaskMembers, thread: &'static Thread) {
        // SAFETY: the thread's creator holds a reference to it, which this
        // handle borrows and never drops.
        let task = ManuallyDrop::new(unsafe { Reference::from_raw(NonNull::from(self)) });
        // The thread's, which it keeps.
        Reference::into_raw(Reference::clone(&task));
        members.thread_count += 1;
        // SAFETY: a thread leaves its task before it is freed.
        members
            .threads
            .push_back(unsafe { UnsafeRef::from_raw(thread) });
    }

    /// Lets `thread`, being freed, out of its `members`; gives back the
    /// reference `add_thread` took for it.
    pub(super) fn remove_thread(
        &self,
        members: &mut TaskMembers,
        thread: &Thread,
    ) -> Reference<Self> {
        members.thread_count -= 1;
        // SAFETY: the thread is on this list, which it joined as it was
        // created.
        unsafe { members.threads.cursor_mut_from_ptr(thread) }.remove();
        // SAFETY: the thread's reference, which it gives up here.
        unsafe { Reference::from_raw(NonNull::from(self)) }
    }

    /// Gives a new send right for its port, with its reference, as
    /// `retrieve_task_self_fast` does.
    ///
    /// # Panics
    ///
    /// If it has no port. If it hands out a right for another port: not
    /// yet. As `Port::copy_send`.
    pub(super) fn retrieve_self(&self) -> Reference<Port> {
        let ports = self.ports.lock();
        let port = ports
            .self_port
            .as_ref()
            .expect("retrieve_task_self_fast: no task port");
        assert!(
            ports
                .self_send
                .as_deref()
                .is_some_and(|send| ptr::eq(send, ptr::from_ref::<Port>(port))),
            "retrieve_task_self_fast: an interposed port, not yet"
        );
        let send = Port::copy_send(port);
        drop(ports);
        send
    }

    /// Sets its special port `which` to `port`, as `task_set_special_port`
    /// does; the old right is released after its lock.
    ///
    /// # Panics
    ///
    /// If it has no port, or `which` is its kernel or bootstrap port: not
    /// yet.
    pub(super) fn set_special_port(&self, which: SpecialPort, port: Option<SendRight>) {
        let mut ports = self.ports.lock();
        assert!(
            ports.self_port.is_some(),
            "task_set_special_port: KERN_FAILURE, not yet"
        );
        let slot = match which {
            SpecialPort::Exception => &mut ports.exception,
            SpecialPort::Kernel => panic!("task_set_special_port: TASK_KERNEL_PORT, not yet"),
            SpecialPort::Bootstrap => panic!("task_set_special_port: TASK_BOOTSTRAP_PORT, not yet"),
        };
        let old = mem::replace(slot, port);
        drop(ports);
        // As ipc_port_release_send.
        drop(old);
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
        if ptr::eq(current.task(), self) {
            current.hold();
            let _section = CriticalSection::enter();
            smp::current().add_asts(Ast::BLOCK);
        }
    }

    /// Lifts one stop users asked for; the last releases it.
    ///
    /// # Errors
    ///
    /// `Failure` if users have not stopped it.
    ///
    /// # Panics
    ///
    /// As `release`.
    pub(super) fn resume(&self) -> Result<(), Error> {
        let mut members = self.members.lock();
        if members.user_stop_count == 0 {
            return Err(Error::Failure);
        }
        members.user_stop_count -= 1;
        let release = members.user_stop_count == 0;
        drop(members);
        if release {
            self.release();
        }
        Ok(())
    }

    /// Lifts one suspension, releasing each of its threads once.
    ///
    /// # Panics
    ///
    /// If it is not alive: not yet. As `Thread::release`.
    fn release(&self) {
        let mut members = self.members.lock();
        assert!(members.active, "task_release: an inactive task, not yet");
        members.suspend_count -= 1;
        let mut cursor = members.threads.front();
        while let Some(thread) = cursor.clone_pointer() {
            // SAFETY: a thread leaves its task before it is freed.
            unsafe { &*UnsafeRef::into_raw(thread) }.release();
            cursor.move_next();
        }
        drop(members);
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

    /// Caps its priority at `max_priority`, as the boot script does with
    /// `host`, the host's port: its priority set to the cap, and its
    /// threads' too.
    ///
    /// # Panics
    ///
    /// If `host` is not a host's port, if the cap is raised, or if it has
    /// threads: not yet.
    pub(super) fn set_max_priority(&self, host: &Port, max_priority: Priority) {
        assert!(
            matches!(
                host.kernel_object(),
                Some(KernelObject::Host(_) | KernelObject::HostPrivileged(_))
            ),
            "task_max_priority: an invalid argument, not yet"
        );
        let mut members = self.members.lock();
        assert!(
            !max_priority.outranks(members.max_priority),
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

// SAFETY: its count is its own, and `free` frees nothing yet.
unsafe impl Counted for Task {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `task`, as `task_deallocate` does at zero.
    ///
    /// # Panics
    ///
    /// Always: not yet.
    unsafe fn free(_task: NonNull<Self>) {
        panic!("task_deallocate: the last reference, not yet");
    }
}

/// The task `port` stands for, with a reference to it taken under the
/// port's lock, as `convert_port_to_task` gives it.
pub fn of(port: &Port) -> Option<Reference<Task>> {
    port.with_kernel_object(|object| match object {
        // SAFETY: a task outlives its port's standing for it.
        Some(KernelObject::Task(task)) => unsafe { Reference::upgrade(*task) },
        _ => None,
    })
}

/// The space of the task `port` stands for, with a reference to it taken
/// under the port's lock, as `convert_port_to_space` gives it.
pub fn space_of(port: &Port) -> Option<Reference<Space>> {
    port.with_kernel_object(|object| match object {
        // SAFETY: a task outlives its port's standing for it.
        Some(KernelObject::Task(task)) => Some(unsafe { task.as_ref() }.space.clone()),
        _ => None,
    })
}

/// The map of the task `port` stands for, with a reference to it taken
/// under the port's lock, as `convert_port_to_map` gives it.
pub fn map_of(port: &Port) -> Option<Reference<Map>> {
    port.with_kernel_object(|object| match object {
        // SAFETY: a task outlives its port's standing for it.
        Some(KernelObject::Task(task)) => Some(unsafe { task.as_ref() }.map.clone()),
        _ => None,
    })
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
    // The kernel task takes the kernel map's one reference.
    let map = KERNEL_MAP.get().expect("task_init: no kernel map");
    let task = create_with_map(map).unwrap_or_else(|error| panic!("task_init: {error:?}"));
    task.members.lock().name.set(format_args!("gnumach"));
    assert!(KERNEL_TASK.set(task).is_ok(), "task_init: twice");
}

/// A new task with no parent over `map`, whose reference the caller gives,
/// named by its address: a new space, and a port with a send right, at the
/// default set's priorities, in the default set.
fn create_with_map(map: &'static Map) -> Result<&'static Task, Error> {
    let mut space = Space::create()?;
    let port = Port::allocate_kernel()?;
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
            // No parent to inherit it from.
            exception: None,
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
            // No higher than the set allows.
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
    // "%p": its address in hex, with no prefix; without the token, as the
    // task is not published yet.
    let address = ptr::from_ref(&*task).addr();
    task.members.get_mut().name.set(format_args!("{address:x}"));
    // The task takes the space's first reference.
    CacheBox::into_raw(space);
    let task = CacheBox::into_raw(task);
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
    DEFAULT_SET.add_task(task);
    Ok(task)
}
