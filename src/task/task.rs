// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, AtomicU64, AtomicUsize, Ordering};

use intrusive_collections::{LinkedListAtomicLink, intrusive_adapter};

use crate::clock::time::AtomicTimeValue;
use crate::ipc::space::Space;
use crate::processor::set::ProcessorSet;
use crate::reference::{Count, Counted, Reference};
use crate::slab::CacheBox;
use crate::sync::{Spinlock, Token};
use crate::vm::map::Map;

use super::create::CACHE;
use super::members::TaskMembers;
use super::ports::TaskPorts;

/// A task; so far only what a new task sets.
#[expect(dead_code, reason = "the task calls read them")]
pub struct Task {
    /// How many references it has.
    pub(super) references: Count,
    /// Its address space.
    pub(crate) map: Reference<Map>,
    /// Its names for ports and port sets.
    pub(crate) space: Reference<Space>,
    /// Its port, and the send right it hands out for it.
    pub(crate) ports: Spinlock<TaskPorts>,
    /// Whether it may be moved to another processor set.
    pub(super) may_assign: bool,
    /// When it was created.
    pub(crate) creation_time: AtomicTimeValue,
    /// The time its ended threads ran in user mode, in microseconds.
    pub(crate) total_user_time: AtomicU64,
    /// The time its ended threads ran in the kernel, in microseconds.
    pub(crate) total_system_time: AtomicU64,
    /// Its page faults: each search for a faulting page counts one.
    pub(crate) faults: AtomicUsize,
    /// Its pages filled with zeros on a fault.
    pub(crate) zero_fills: AtomicUsize,
    /// Its pages asked of a pager on a fault.
    pub(crate) pageins: AtomicUsize,
    /// Its pages copied up from a shadow object on a write fault.
    pub(crate) cow_faults: AtomicUsize,
    /// The messages it sent.
    pub(crate) messages_sent: AtomicUsize,
    /// The messages it received.
    pub(crate) messages_received: AtomicUsize,
    /// Whether it is alive, its priorities and suspensions, its threads and
    /// its name.
    pub(crate) members: Token<TaskMembers>,
    /// The processor set its threads run on; set as the set takes it.
    pub(crate) set: AtomicPtr<ProcessorSet>,
    /// Its place in its set's tasks.
    pub(super) set_link: LinkedListAtomicLink,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub SetTaskAdapter = &'static Task: Task { set_link => LinkedListAtomicLink }
);

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
