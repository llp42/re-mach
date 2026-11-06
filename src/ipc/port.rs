// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Ports.

use core::ptr::NonNull;
use core::sync::atomic::{AtomicU32, Ordering};

use super::space::{self, Space};
use crate::kernel::Error;
use crate::kernel::host::Host;
use crate::kernel::processor::{Processor, ProcessorSet};
use crate::kernel::slab::{Cache, CacheBox};
use crate::kernel::task::Task;
use crate::kernel::thread::Thread;
use crate::sync::Spinlock;
use crate::vm::object::Object;

/// A port: its reference count, and its state under its lock.
pub struct Port {
    /// How many references it has.
    references: AtomicU32,
    /// Its state.
    pub(super) state: Spinlock<PortState>,
}

/// A port's state under its lock; so far only whether it is alive, its
/// receiver, its send rights and what it stands for.
pub(super) struct PortState {
    /// Whether it is alive.
    pub(super) active: bool,
    /// The space that holds its receive right.
    pub(super) receiver: NonNull<Space>,
    /// How many send rights for it there are.
    send_rights: u32,
    /// How many send rights have been made from its receive right.
    make_send_count: u32,
    /// What it stands for, when the kernel receives on it.
    kernel_object: Option<KernelObject>,
}

impl Port {
    /// Makes it stand for `object`.
    pub fn set_kernel_object(&self, object: KernelObject) {
        self.state.lock().kernel_object = Some(object);
    }

    /// What it stands for, while it is alive.
    pub fn kernel_object(&self) -> Option<KernelObject> {
        let state = self.state.lock();
        let object = if state.active {
            state.kernel_object
        } else {
            None
        };
        drop(state);
        object
    }

    /// Makes a send right for it from its receive right, which holds a
    /// reference.
    pub fn make_send(&self) {
        let mut state = self.state.lock();
        state.make_send_count += 1;
        state.send_rights += 1;
        drop(state);
        self.references.fetch_add(1, Ordering::Relaxed);
    }
}

/// What a port the kernel receives on stands for; only the kinds used here.
#[derive(Clone, Copy)]
pub enum KernelObject {
    /// A memory object's name port, which names it to `vm_region`.
    PagerName(#[expect(dead_code, reason = "vm_object_lookup_name reads it")] NonNull<Object>),
    /// The host's port.
    Host(#[expect(dead_code, reason = "convert_port_to_host reads it")] &'static Host),
    /// The host's privileged port.
    HostPrivileged(
        #[expect(dead_code, reason = "convert_port_to_host_priv reads it")] &'static Host,
    ),
    /// A processor set's port.
    ProcessorSet(
        #[expect(dead_code, reason = "convert_port_to_pset reads it")] NonNull<ProcessorSet>,
    ),
    /// A processor set's name port.
    ProcessorSetName(
        #[expect(dead_code, reason = "convert_port_to_pset_name reads it")] NonNull<ProcessorSet>,
    ),
    /// A processor's port.
    Processor(
        #[expect(dead_code, reason = "convert_port_to_processor reads it")] &'static Processor,
    ),
    /// A processor's name port.
    ProcessorName(#[expect(dead_code, reason = "processor_info reads it")] &'static Processor),
    /// A task's port.
    Task(#[expect(dead_code, reason = "convert_port_to_task reads it")] NonNull<Task>),
    /// A thread's port.
    Thread(#[expect(dead_code, reason = "convert_port_to_thread reads it")] NonNull<Thread>),
}

/// The cache ports come from.
pub(super) static CACHE: Cache<Port> = Cache::new("ipc_port");

/// A new port the kernel receives on, with one reference.
///
/// # Errors
///
/// `ResourceShortage` if no port can be allocated.
pub fn allocate_kernel() -> Result<CacheBox<Port>, Error> {
    CACHE.allocate(Port {
        references: AtomicU32::new(1),
        state: Spinlock::new(PortState {
            active: true,
            receiver: NonNull::from(&space::KERNEL_SPACE),
            send_rights: 0,
            make_send_count: 0,
            kernel_object: None,
        }),
    })
}
