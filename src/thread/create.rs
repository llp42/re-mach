// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem;
use core::ptr::{self, NonNull};
use core::sync::atomic::{
    AtomicBool, AtomicI32, AtomicPtr, AtomicU8, AtomicU32, AtomicU64, AtomicUsize, Ordering,
};

use intrusive_collections::UnsafeRef;
use tailq::Link;

use crate::arch;
use crate::clock;
use crate::clock::timeout::Timeout;
use crate::error::Error;
use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::name::Name;
use crate::processor::all_sets::DEFAULT_SET;
use crate::reference::{Count, Reference};
use crate::sched;
use crate::sched::policy::{Policy, SCHEDULER_SCALE};
use crate::sched::wait::WaitResult;
use crate::slab::{Cache, CacheBox};
use crate::sync::{CriticalSection, OnceLock, Spinlock};
use crate::task::task::Task;
use crate::vm;

use super::machine::MachineThread as _;
use super::ports::ThreadPorts;
use super::state::ThreadState;
use super::thread::{TIMER_RATE, Thread};
use super::timeout::thread_timeout;

/// The cache threads come from.
pub(super) static CACHE: Cache<Thread> = Cache::new("thread");

/// Where a new thread begins, before its function: it dispatches `old`, the
/// thread switched from if any, then leaves the critical section the switch
/// to it was made in.
pub extern "sysv64" fn begin(old: Option<&'static Thread>) {
    if let Some(old) = old {
        sched::switch::dispatch(old);
    }
    // SAFETY: a switch is made in a critical section no guard holds, and
    // this thread runs its first instructions in it.
    drop(unsafe { CriticalSection::inherit() });
}

/// Sets up threads, on the boot CPU.
pub fn init() {
    CACHE.register();
    arch::MachineState::module_init();
}

impl Thread {
    /// A new thread of `task`, with a reference for the caller, suspended,
    /// with the kernel stack it keeps for life, in its task's set, or the
    /// default set if that one is not in use, at its task's priority aged by a
    /// full share of a processor, with a port and a send right for it; a
    /// thread made in a set with no processor is suspended once more.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if the thread, its machine state or its port can't
    /// be allocated, or as `vm::kernel::allocate_stack` for its stack;
    /// `Failure` if the task is not alive.
    #[expect(
        clippy::too_many_lines,
        reason = "thread_create's steps, in GNU Mach's order"
    )]
    pub fn create(task: &'static Task) -> Result<&'static Self, Error> {
        let mut machine_state = arch::MachineState::new()?;
        let port = Port::allocate_kernel()?;
        // A full share of a processor.
        let sched_usage = TIMER_RATE * SCHEDULER_SCALE;
        let mut thread = CACHE.allocate(Self {
            sleep_link: Link::new(),
            run_link: Link::new(),
            task_link: Link::new(),
            set_link: Link::new(),
            ipc_link: Link::new(),
            // One for being alive, one for the caller.
            references: Count::new(2),
            // SAFETY: a task lives while it has threads.
            task: unsafe { UnsafeRef::from_raw(task) },
            state: AtomicU32::new(ThreadState::SUSPENDED.bits()),
            wait_event: AtomicUsize::new(0),
            wait_result: AtomicU8::new(WaitResult::Awakened.number()),
            timer: Timeout::new(thread_timeout),
            // Set below with the priorities, under the set's and task's tokens.
            suspend_count: AtomicU32::new(0),
            user_stop_count: AtomicU32::new(1),
            priority: AtomicU32::new(0),
            max_priority: AtomicU32::new(0),
            sched_pri: AtomicU32::new(0),
            policy: Policy::Timeshare,
            // Set below, by its set's load average.
            cpu_usage: AtomicU32::new(0),
            sched_usage: AtomicU32::new(sched_usage),
            sched_stamp: AtomicU32::new(sched::tick::scheduler_tick()),
            active: AtomicBool::new(true),
            set: AtomicPtr::new(ptr::null_mut()),
            may_assign: true,
            creation_time: clock::time::stamp(),
            // Its task's, copied below under the task's token.
            name: Name::new(),
            // SAFETY: its box, unused until given up below, outlives the thread.
            machine_state: unsafe { UnsafeRef::from_raw(ptr::from_mut(&mut *machine_state)) },
            self_port: AtomicPtr::new(ptr::null_mut()),
            self_send: AtomicPtr::new(ptr::null_mut()),
            // As ipc_thread_init: no exception port, no reply port.
            ports: Spinlock::new(ThreadPorts {
                exception: None,
                reply: None,
            }),
            argument: AtomicPtr::new(ptr::null_mut()),
            kernel_stack: OnceLock::new(),
            start_function: OnceLock::new(),
            last_processor: AtomicPtr::new(ptr::null_mut()),
            wake_active: AtomicBool::new(false),
            bound: AtomicPtr::new(ptr::null_mut()),
            critical_nesting: AtomicU32::new(1),
            user_time: AtomicU64::new(0),
            system_time: AtomicU64::new(0),
            charged_user_time: AtomicU64::new(0),
            charged_system_time: AtomicU64::new(0),
            cpu_delta: AtomicU32::new(0),
            sched_delta: AtomicU32::new(0),
            depressed_from: AtomicI32::new(-1),
            fixed_quantum: 0,
            asts: AtomicU32::new(0),
        })?;
        let stack = vm::kernel::allocate_stack()?;
        assert!(
            thread.kernel_stack.set(stack).is_ok(),
            "thread_create: a stack already"
        );
        let members = task.members.lock();
        // SAFETY: under the task's token its reference keeps its set alive,
        // which this handle borrows and never drops; the one taken here does
        // after.
        let mut set = Reference::clone(&mem::ManuallyDrop::new(unsafe {
            Reference::from_raw(NonNull::new(task.set.load(Ordering::Acquire)).unwrap())
        }));
        drop(members);
        // The set's token, then the task's, again if the task changed sets; a
        // set out of use gives way to the default set.
        let (mut set_members, mut members) = loop {
            let set_members = set.lock();
            let members = task.members.lock();
            // SAFETY: as above; a new set gets its reference before the tokens
            // drop.
            let current = unsafe { &*task.set.load(Ordering::Acquire) };
            let current = if current.is_active() {
                current
            } else {
                &DEFAULT_SET
            };
            if ptr::eq(current, Reference::as_ptr(&set).as_ptr()) {
                break (set_members, members);
            }
            // SAFETY: as above, under both tokens.
            let current = Reference::clone(&mem::ManuallyDrop::new(unsafe {
                Reference::from_raw(NonNull::from(current))
            }));
            drop(members);
            drop(set_members);
            // Never the last: a set keeps its own reference until destroyed.
            set = current;
        };
        // Its share at the set's load average, taken as at least 1.
        let load_average = set.load_average().max(i64::from(SCHEDULER_SCALE));
        *thread.cpu_usage.get_mut() = u32::try_from(i64::from(sched_usage) / load_average).unwrap();
        let max_priority = members.max_priority.weaker(set.max_priority());
        *thread.max_priority.get_mut() = u32::from(max_priority);
        let priority = members.priority.weaker(max_priority);
        *thread.priority.get_mut() = u32::from(priority);
        *thread.sched_pri.get_mut() = u32::from(priority.aged_by(sched_usage));
        *thread.suspend_count.get_mut() = members.suspend_count + 1 + u32::from(set.is_empty());
        thread.name.copy_from(&members.name);
        // Nothing is published yet; only the set's reference is undone, and
        // the stack stays taken: this path does not free it.
        if !members.active {
            drop(members);
            drop(set_members);
            drop(set);
            return Err(Error::Failure);
        }
        // The thread keeps its machine state for life.
        CacheBox::into_raw(machine_state);
        let thread = CacheBox::into_raw(thread);
        // The box's provenance, which `free` takes back from a handle made
        // from a shared borrow.
        thread.expose_provenance();
        // SAFETY: a thread lives until its last reference drops.
        let thread = unsafe { thread.as_ref() };
        thread.timer.set_argument(ptr::from_ref(thread).cast());
        let port = Reference::from(port);
        let send = Port::make_send(&port);
        port.set_kernel_object(KernelObject::Thread(NonNull::from(thread)));
        // Each slot keeps its reference.
        thread
            .self_port
            .store(Reference::into_raw(port).as_ptr(), Ordering::Release);
        thread
            .self_send
            .store(Reference::into_raw(send).as_ptr(), Ordering::Release);
        set.add_thread(&mut set_members, thread);
        task.add_thread(&mut members, thread);
        drop(members);
        drop(set_members);
        // The thread keeps the set's reference.
        Reference::into_raw(set);
        Ok(thread)
    }

    /// Makes it start in `function` once it first runs: its stack is set to
    /// enter it, and the function kept, as `thread_start` keeps it.
    ///
    /// # Panics
    ///
    /// If it has no stack, which `create` gives it, or was started already.
    pub fn start(&self, function: fn() -> !) {
        let stack = self.kernel_stack.get().expect("thread_start: no stack");
        self.machine_state.attach_stack(stack, function);
        assert!(
            self.start_function.set(function).is_ok(),
            "thread_start: started already"
        );
    }
}

/// Makes a thread of `task` that starts in `start`, at the system priority,
/// its highest too, and resumes it; the reference its creation gave the
/// caller is dropped.
///
/// # Errors
///
/// As `Thread::create`.
///
/// # Panics
///
/// If it cannot be resumed: a new thread always can.
pub fn kernel_thread(task: &'static Task, start: fn() -> !) -> Result<(), Error> {
    let thread = Thread::create(task)?;
    // SAFETY: the reference its creation gave the caller, dropped here; its
    // own keeps it alive.
    drop(unsafe { Reference::from_raw(NonNull::from(thread)) });
    thread.start(start);
    // Never run, it waits on no run queue.
    let priority = u32::from(sched::priority::SYSTEM_PRIORITY);
    thread.max_priority.store(priority, Ordering::Relaxed);
    thread.priority.store(priority, Ordering::Relaxed);
    thread.sched_pri.store(priority, Ordering::Relaxed);
    thread
        .resume()
        .unwrap_or_else(|error| panic!("kernel_thread: thread_resume: {error:?}"));
    Ok(())
}
