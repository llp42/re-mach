// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::{ManuallyDrop, MaybeUninit};
use core::ptr::{self, NonNull};
use core::slice;

use intrusive_collections::LinkedList;

use crate::error::Error;
use crate::kalloc;
use crate::name::Name;
use crate::reference::Reference;
use crate::sched::Priority;
use crate::thread::thread::{TaskThreadAdapter, Thread};

use super::task::Task;

/// Whether a task is alive, its priorities and suspensions, its threads and
/// its name.
pub struct TaskMembers {
    /// Whether the task is alive.
    pub(crate) active: bool,
    /// The priority of the task's threads.
    pub(crate) priority: Priority,
    /// The highest priority the task's threads may take.
    pub(crate) max_priority: Priority,
    /// How many times the task is suspended.
    pub(crate) suspend_count: u32,
    /// How many times users stopped the task; the first suspends it.
    pub(super) user_stop_count: u32,
    /// The threads, in the order the task took them.
    pub(super) threads: LinkedList<TaskThreadAdapter>,
    /// How many threads there are.
    pub(super) thread_count: u32,
    /// The task's name, ending with a NUL.
    pub(crate) name: Name,
}

impl Task {
    /// Takes `thread`, new, into its `members`, with a reference for it.
    pub(crate) fn add_thread(&self, members: &mut TaskMembers, thread: &'static Thread) {
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
    pub(crate) fn remove_thread(
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
    pub(crate) fn thread_ports(&self) -> Result<Option<NonNull<[usize]>>, Error> {
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
}
