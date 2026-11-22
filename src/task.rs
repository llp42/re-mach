// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Tasks: an address space, a name space for ports, and threads.

use core::mem::{self, ManuallyDrop, MaybeUninit};
use core::ptr::{self, NonNull};
use core::slice;
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use intrusive_collections::{LinkedList, LinkedListAtomicLink, intrusive_adapter};

use crate::arch::{self, MAX_USER_ADDRESS, MIN_USER_ADDRESS, Machine as _, PAGE_SIZE};
use crate::ast::Ast;
use crate::clock;
use crate::clock::time::{AtomicTimeValue, TimeValue};
use crate::error::Error;
use crate::ipc::port::{KernelObject, Port, SendRight};
use crate::ipc::space::Space;
use crate::kalloc;
use crate::name::Name;
use crate::processor::{DEFAULT_SET, ProcessorSet};
use crate::reference::{Count, Counted, Reference};
use crate::sched::{BasePriority, Priority, USER_PRIORITY};
use crate::slab::{Cache, CacheBox};
use crate::smp;
use crate::sync::{CriticalSection, OnceLock, Spinlock, Token};
use crate::thread::{RpcTimeValue, TIMER_RATE, TaskThreadAdapter, Thread, TimeValue64};
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
    pub SetTaskAdapter = &'static Task: Task { set_link => LinkedListAtomicLink }
);

/// A task's ports, under its lock.
pub struct TaskPorts {
    /// Its port, which controls it.
    pub(super) self_port: Option<Reference<Port>>,
    /// The send right for its port it hands out.
    self_send: Option<Reference<Port>>,
    /// The send right its threads' exceptions go to, if any.
    pub(super) exception: Option<SendRight>,
    /// The send right for its bootstrap port, if any.
    bootstrap: Option<SendRight>,
    /// The send rights registered for it, Mach's `TASK_PORT_REGISTER_MAX`.
    registered: [Option<SendRight>; 4],
}

/// What memory a new task starts with.
#[derive(Clone, Copy)]
pub enum Memory {
    /// A copy of its parent's, as each of the parent's entries' inheritance
    /// says.
    Inherited,
    /// None.
    Empty,
}

/// Whether a task's priority change reaches its threads.
#[derive(Clone, Copy)]
pub enum Threads {
    /// They keep theirs.
    Kept,
    /// They take it too.
    Changed,
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

/// Mach's `task_basic_info`: 30 words.
#[repr(C)]
struct BasicInfo {
    /// The stops users asked for.
    suspend_count: i32,
    base_priority: i32,
    /// How much its map maps, in bytes.
    virtual_size: usize,
    /// How much its physical map maps, in bytes.
    resident_size: usize,
    /// Its ended threads' time in user mode.
    user_time: RpcTimeValue,
    /// Its ended threads' time in the kernel.
    system_time: RpcTimeValue,
    creation_time: RpcTimeValue,
    user_time64: TimeValue64,
    system_time64: TimeValue64,
    creation_time64: TimeValue64,
}

/// Mach's `task_events_info`: 14 words.
#[repr(C)]
struct EventsInfo {
    faults: usize,
    zero_fills: usize,
    reactivations: usize,
    pageins: usize,
    cow_faults: usize,
    messages_sent: usize,
    messages_received: usize,
}

/// Mach's `task_thread_times_info`: 16 words.
#[repr(C)]
struct ThreadTimesInfo {
    /// Its live threads' time in user mode.
    user_time: RpcTimeValue,
    /// Its live threads' time in the kernel.
    system_time: RpcTimeValue,
    user_time64: TimeValue64,
    system_time64: TimeValue64,
}

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

    /// Takes `thread`, new, into its `members`, with a reference for it.
    pub(super) fn add_thread(&self, members: &mut TaskMembers, thread: &'static Thread) {
        // SAFETY: the thread's creator holds a reference to it, which this
        // handle borrows and never drops.
        let task = ManuallyDrop::new(unsafe { Reference::from_raw(NonNull::from(self)) });
        // The thread's, which it keeps.
        Reference::into_raw(Reference::clone(&task));
        members.thread_count += 1;
        members.threads.push_back(thread);
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

    /// A send right for `task`'s port, made as `convert_task_to_port` makes
    /// it; `task`'s reference drops after.
    ///
    /// # Panics
    ///
    /// If it has no port: not yet.
    pub(super) fn convert_to_port(task: Reference<Self>) -> Reference<Port> {
        let ports = task.ports.lock();
        let port = ports
            .self_port
            .as_ref()
            .unwrap_or_else(|| panic!("convert_task_to_port: no port, not yet"));
        let send = Port::make_send(port);
        drop(ports);
        drop(task);
        send
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

    /// Makes its port stand for nothing, as `ipc_task_disable` does.
    fn disable_ipc(&self) {
        let ports = self.ports.lock();
        if let Some(port) = &ports.self_port {
            port.replace_kernel_object(None);
        }
        drop(ports);
    }

    /// Ends its ipc, as `ipc_task_terminate` does: its slots emptied, its
    /// naked send rights released, its space destroyed and its port
    /// destroyed; the names tasks hold for the port stay, as rights to a dead
    /// port.
    ///
    /// # Panics
    ///
    /// If it has no port: not yet. As `Space::destroy` and
    /// `Port::deallocate_kernel`.
    fn terminate_ipc(&self) {
        let mut ports = self.ports.lock();
        let port = ports
            .self_port
            .take()
            .unwrap_or_else(|| panic!("ipc_task_terminate: an ended task, not yet"));
        let send = ports.self_send.take();
        let exception = ports.exception.take();
        let bootstrap = ports.bootstrap.take();
        let registered = mem::take(&mut ports.registered);
        drop(ports);
        // Released after the lock, in GNU Mach's order.
        if let Some(send) = send {
            Port::release_send(send);
        }
        drop(exception);
        drop(bootstrap);
        drop(registered);
        self.space.destroy();
        port.deallocate_kernel();
        drop(port);
    }

    /// Gives send rights for its threads' ports, in the order it took them,
    /// as `task_threads` does: a kalloc array of the ports' exposed
    /// addresses, each holding its right's reference, sized with its token
    /// released; none for no threads.
    ///
    /// # Errors
    ///
    /// As `kalloc::allocate`.
    ///
    /// # Panics
    ///
    /// If it is not alive, a thread is being freed, or threads left while
    /// the array was allocated: not yet. As `Thread::convert_to_port`.
    pub(super) fn thread_ports(&self) -> Result<Option<NonNull<[usize]>>, Error> {
        let mut array: Option<NonNull<[usize]>> = None;
        // Allocated with the token released, until it holds every thread.
        let members = loop {
            let members = self.members.lock();
            assert!(members.active, "task_threads: an inactive task, not yet");
            let count = usize::try_from(members.thread_count).unwrap();
            if count <= array.map_or(0, NonNull::len) {
                break members;
            }
            drop(members);
            if let Some(array) = array {
                // SAFETY: from `kalloc::allocate` of its size, below; not used
                // again.
                unsafe { kalloc::free(array.cast(), array.len() * size_of::<usize>()) };
            }
            let memory = kalloc::allocate(count * size_of::<usize>())?;
            array = Some(NonNull::slice_from_raw_parts(memory.cast(), count));
        };
        let Some(array) = array else {
            drop(members);
            return Ok(None);
        };
        assert!(
            usize::try_from(members.thread_count) == Ok(array.len()),
            "task_threads: fewer threads than the array holds, not yet"
        );
        // SAFETY: the array's slots, ours alone.
        let slots = unsafe {
            slice::from_raw_parts_mut(array.cast::<MaybeUninit<usize>>().as_ptr(), array.len())
        };
        // A reference to each thread, for its port.
        for (slot, thread) in slots.iter_mut().zip(&members.threads) {
            // SAFETY: a thread leaves its task before it is freed.
            let thread = unsafe { Reference::upgrade(NonNull::from(thread)) }
                .expect("task_threads: a thread being freed, not yet");
            slot.write(Reference::into_raw(thread).as_ptr().expose_provenance());
        }
        drop(members);
        // Each thread's reference becomes its port's, as
        // convert_thread_to_port.
        for slot in slots {
            // SAFETY: written above with a thread's reference given up by
            // `into_raw`, which the handle takes, as the slot is overwritten
            // below.
            let thread = unsafe {
                Reference::from_raw(
                    NonNull::new(ptr::with_exposed_provenance_mut::<Thread>(
                        slot.assume_init_read(),
                    ))
                    .unwrap(),
                )
            };
            let port = Thread::convert_to_port(thread);
            slot.write(Reference::into_raw(port).as_ptr().expose_provenance());
        }
        Ok(Some(array))
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
        let mut members = self.members.lock();
        Self::hold(&mut members);
        drop(members);
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

    /// Ends it, as `task_terminate` does for a task other than the current
    /// one: no longer alive, held and waited for, its port made to stand for
    /// nothing and its ipc ended, then its reference for being alive
    /// dropped; the caller's keeps it until the call ends.
    ///
    /// # Panics
    ///
    /// If no thread runs on the CPU. If it is the current task, it or the
    /// current task or thread is no longer alive, or it has threads: not
    /// yet. As `wait_until_stopped` and `terminate_ipc`.
    pub(super) fn terminate(&self) {
        let current = smp::current()
            .active_thread()
            .expect("task_terminate: no current thread");
        assert!(
            !ptr::eq(current.task(), self),
            "task_terminate: the current task, not yet"
        );
        let current_members = current.task().members.lock();
        assert!(
            current_members.active && current.is_active(),
            "task_terminate: the current task or thread being ended, not yet"
        );
        drop(current_members);
        let mut members = self.members.lock();
        assert!(
            members.active,
            "task_terminate: a task being ended, not yet"
        );
        Self::hold(&mut members);
        members.active = false;
        drop(members);
        self.wait_until_stopped();
        self.disable_ipc();
        let members = self.members.lock();
        assert!(
            members.threads.is_empty(),
            "task_terminate: thread_force_terminate, not yet"
        );
        drop(members);
        self.terminate_ipc();
        // task_deallocate: the reference of its being alive.
        // SAFETY: an alive task holds that reference, given up here, once, as
        // it is no longer alive; the caller's keeps it until the call ends.
        drop(unsafe { Reference::from_raw(NonNull::from(self)) });
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
            thread.release();
            cursor.move_next();
        }
        drop(members);
    }

    /// Suspends the task of `members` once more, holding its threads but the
    /// current one, as `task_hold_locked` does.
    ///
    /// # Panics
    ///
    /// If it is not alive: not yet.
    fn hold(members: &mut TaskMembers) {
        let current = smp::current()
            .active_thread()
            .expect("task_hold: no current thread");
        assert!(members.active, "task_hold: an inactive task, not yet");
        members.suspend_count += 1;
        for thread in &members.threads {
            if !ptr::eq(thread, current) {
                thread.hold();
            }
        }
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

    /// Writes its basic information into `info`, as `task_info`'s
    /// `TASK_BASIC_INFO` does; `info`'s length is the count asked for.
    /// Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a count short of the record without its 64-bit
    /// times.
    pub(super) fn basic_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        let count = size_of::<BasicInfo>() / size_of::<i32>();
        let short_count = mem::offset_of!(BasicInfo, user_time64) / size_of::<i32>();
        if info.len() < short_count {
            return Err(Error::InvalidArgument);
        }
        let virtual_size = self.map.size();
        let resident_size = self.map.phys_map().resident_count() * PAGE_SIZE;
        let time = |microseconds: &AtomicU64| {
            let microseconds = microseconds.load(Ordering::Relaxed);
            TimeValue {
                seconds: i64::try_from(microseconds / u64::from(TIMER_RATE)).unwrap(),
                nanoseconds: i64::try_from(microseconds % u64::from(TIMER_RATE) * 1000).unwrap(),
            }
        };
        let members = self.members.lock();
        let user_time = time(&self.total_user_time);
        let system_time = time(&self.total_system_time);
        // As read_time_stamp; no host_set_time yet, so no offset.
        let creation_time = self.creation_time.load();
        let basic = BasicInfo {
            suspend_count: i32::try_from(members.user_stop_count).unwrap(),
            base_priority: i32::try_from(u32::from(members.priority)).unwrap(),
            virtual_size,
            resident_size,
            user_time: user_time.into(),
            system_time: system_time.into(),
            creation_time: creation_time.into(),
            user_time64: user_time.into(),
            system_time64: system_time.into(),
            creation_time64: creation_time.into(),
        };
        drop(members);
        // The 64-bit times only for the whole record's count, as Mach's.
        let written = if info.len() == count {
            count
        } else {
            short_count
        };
        // SAFETY: `count` words of integers, `repr(C)` with no implicit
        // padding.
        let words = unsafe { slice::from_raw_parts(ptr::from_ref(&basic).cast::<i32>(), written) };
        info[..written].copy_from_slice(words);
        Ok(info.len().min(count))
    }

    /// Writes its event counts into `info`, as `task_info`'s
    /// `TASK_EVENTS_INFO` does; `info`'s length is the count asked for.
    /// Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a count short of the record.
    pub(super) fn events_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        let count = size_of::<EventsInfo>() / size_of::<i32>();
        if info.len() < count {
            return Err(Error::InvalidArgument);
        }
        // Counted without its token, so read without it.
        let events = EventsInfo {
            faults: self.faults.load(Ordering::Relaxed),
            zero_fills: self.zero_fills.load(Ordering::Relaxed),
            // Counted only by page eviction: not yet.
            reactivations: 0,
            pageins: self.pageins.load(Ordering::Relaxed),
            cow_faults: self.cow_faults.load(Ordering::Relaxed),
            messages_sent: self.messages_sent.load(Ordering::Relaxed),
            messages_received: self.messages_received.load(Ordering::Relaxed),
        };
        // SAFETY: `count` words of integers, `repr(C)` with no padding.
        let words = unsafe { slice::from_raw_parts(ptr::from_ref(&events).cast::<i32>(), count) };
        info[..count].copy_from_slice(words);
        Ok(count)
    }

    /// Writes the times of its live threads into `info`, as `task_info`'s
    /// `TASK_THREAD_TIMES_INFO` does; `info`'s length is the count asked
    /// for. Gives the count written.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` for a count short of the record without its 64-bit
    /// times.
    pub(super) fn thread_times_info(&self, info: &mut [i32]) -> Result<usize, Error> {
        let count = size_of::<ThreadTimesInfo>() / size_of::<i32>();
        let short_count = mem::offset_of!(ThreadTimesInfo, user_time64) / size_of::<i32>();
        if info.len() < short_count {
            return Err(Error::InvalidArgument);
        }
        // As time_value64_add.
        let add = |sum: TimeValue, time: TimeValue| {
            TimeValue {
                seconds: sum.seconds + time.seconds,
                nanoseconds: sum.nanoseconds,
            }
            .add_nanoseconds(time.nanoseconds)
        };
        let zero = TimeValue {
            seconds: 0,
            nanoseconds: 0,
        };
        let (mut user_time, mut system_time) = (zero, zero);
        let members = self.members.lock();
        for thread in &members.threads {
            let (thread_user_time, thread_system_time) = thread.read_times();
            user_time = add(user_time, thread_user_time);
            system_time = add(system_time, thread_system_time);
        }
        drop(members);
        let times = ThreadTimesInfo {
            user_time: user_time.into(),
            system_time: system_time.into(),
            user_time64: user_time.into(),
            system_time64: system_time.into(),
        };
        // The 64-bit times only for a count holding them, as Mach's.
        let written = if info.len() >= count {
            count
        } else {
            short_count
        };
        // SAFETY: `count` words of integers, `repr(C)` with no implicit
        // padding.
        let words = unsafe { slice::from_raw_parts(ptr::from_ref(&times).cast::<i32>(), written) };
        info[..written].copy_from_slice(words);
        Ok(info.len().min(count))
    }

    /// Sets its priority, which its new threads take, to `priority`, and its
    /// threads' as `threads` says, as `task_priority` does.
    ///
    /// # Errors
    ///
    /// `NoAccess` if `priority` outranks its cap; `Failure` if a thread's
    /// cap refuses it, the other threads changed still.
    ///
    /// # Panics
    ///
    /// As `Thread::try_set_priority`.
    pub(super) fn set_priority(&self, priority: Priority, threads: Threads) -> Result<(), Error> {
        let mut members = self.members.lock();
        if priority.outranks(members.max_priority) {
            return Err(Error::NoAccess);
        }
        members.priority = priority;
        let mut result = Ok(());
        if matches!(threads, Threads::Changed) {
            for thread in &members.threads {
                if thread.try_set_priority(priority).is_err() {
                    result = Err(Error::Failure);
                }
            }
        }
        drop(members);
        result
    }

    /// Caps its priority at `max_priority` for `host`, a host's port: its
    /// priority as `base` says, and its threads' as `threads` says, as
    /// `task_max_priority` does.
    ///
    /// # Errors
    ///
    /// `InvalidArgument` unless `host` is a host's port, alive; `NoAccess`
    /// if the cap is raised and `host` is not the privileged one.
    ///
    /// # Panics
    ///
    /// As `Thread::override_max_priority`.
    pub(super) fn set_max_priority(
        &self,
        host: &Port,
        max_priority: Priority,
        base: BasePriority,
        threads: Threads,
    ) -> Result<(), Error> {
        let privileged = match host.kernel_object() {
            Some(KernelObject::Host(_)) => false,
            Some(KernelObject::HostPrivileged(_)) => true,
            _ => return Err(Error::InvalidArgument),
        };
        let mut members = self.members.lock();
        if max_priority.outranks(members.max_priority) && !privileged {
            return Err(Error::NoAccess);
        }
        members.max_priority = max_priority;
        if members.priority.outranks(max_priority) || matches!(base, BasePriority::Set) {
            members.priority = max_priority;
        }
        if matches!(threads, Threads::Changed) {
            for thread in &members.threads {
                thread.override_max_priority(max_priority, base);
            }
        }
        drop(members);
        Ok(())
    }

    /// Names it `name`, up to its first NUL, cut to fit.
    pub(super) fn set_name(&self, name: impl IntoIterator<Item = u8>) {
        self.members.lock().name.set_bytes(name);
    }
}

// SAFETY: its count is its own, and every task but the kernel's, whose own
// references never drop, is a box of `CACHE`, which `free` takes back.
unsafe impl Counted for Task {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `task`, as `task_deallocate` does at zero: it leaves its set,
    /// its map's and space's references drop, then it goes back to its
    /// cache.
    ///
    /// # Panics
    ///
    /// As `ProcessorSet::remove_task` and the map's free.
    unsafe fn free(task: NonNull<Self>) {
        // The box's own provenance, which `create_with_map` exposed.
        let task = NonNull::<Self>::with_exposed_provenance(task.addr());
        // SAFETY: nothing else uses it, and it stays allocated until its
        // box drops below.
        let this = unsafe { task.as_ref() };
        // machine_task_terminate: a task has no I/O permission bitmap yet.
        // eml_task_deallocate: no emulation vector exists.
        // SAFETY: a task's set lives while the task is in it.
        let set = unsafe { &*this.set.load(Ordering::Acquire) };
        set.remove_task(this);
        // Never the last: a set keeps its own reference until destroyed.
        // SAFETY: the task's reference, taken in `create_with_map`, given up
        // here.
        drop(unsafe { Reference::from_raw(NonNull::from(set)) });
        // The box drops its fields in order: the map's reference, as
        // vm_map_deallocate, then the space's, as is_release.
        // SAFETY: a `CACHE` box `create_with_map` gave up, with that box's
        // provenance; nothing uses it after.
        drop(unsafe { CacheBox::from_raw(&CACHE, task) });
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
