// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Ports.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU32, Ordering};

use super::space::{self, Space};
use super::{MessageType, object};
use crate::error::Error;
use crate::host::Host;
use crate::processor::{Processor, ProcessorSet};
use crate::slab::{Cache, CacheBox};
use crate::sync::Spinlock;
use crate::task::Task;
use crate::thread::Thread;
use crate::vm::object::Object;

/// A port: its reference count, and its state under its lock.
pub struct Port {
    /// How many references it has.
    references: AtomicU32,
    /// Its state.
    pub(super) state: Spinlock<PortState>,
}

/// A port's state under its lock; so far only whether it is alive, its
/// receiver and its name there, its send and send-once rights and what it
/// stands for.
pub(super) struct PortState {
    /// Whether it is alive.
    pub(super) active: bool,
    /// The space that holds its receive right.
    pub(super) receiver: NonNull<Space>,
    /// Its receive right's name in that space.
    pub(super) receiver_name: PortName,
    /// How many send rights for it there are.
    pub(super) send_rights: u32,
    /// How many send-once rights for it there are.
    pub(super) send_once_rights: u32,
    /// How many send rights have been made from its receive right.
    make_send_count: u32,
    /// What it stands for, when the kernel receives on it.
    kernel_object: Option<KernelObject>,
}

impl Port {
    /// A new live port that `receiver` receives on, with one reference and
    /// no rights out, as `ipc_port_init` sets it up.
    const fn new(receiver: NonNull<Space>) -> Self {
        Self {
            references: AtomicU32::new(1),
            state: Spinlock::new(PortState {
                active: true,
                receiver,
                receiver_name: PortName::NULL,
                send_rights: 0,
                send_once_rights: 0,
                make_send_count: 0,
                kernel_object: None,
            }),
        }
    }

    /// A new port the kernel receives on, with one reference.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no port can be allocated.
    pub fn allocate_kernel() -> Result<CacheBox<Self>, Error> {
        let mut port = CACHE.allocate(Self::new(NonNull::from(&space::KERNEL_SPACE)))?;
        // Its address's low 32 bits, as GNU Mach names kernel ports.
        let address = ptr::from_ref(&*port).addr() & 0xffff_ffff;
        port.state.get_mut().receiver_name = PortName::from(u32::try_from(address).unwrap());
        Ok(port)
    }

    /// A new port that `space` receives on, under the lowest free name there,
    /// its one reference the receive right's; gives the name.
    ///
    /// # Errors
    ///
    /// As `object::allocate`.
    ///
    /// # Panics
    ///
    /// As `object::allocate`.
    pub fn allocate(space: &Space) -> Result<PortName, Error> {
        let port = CACHE.allocate(Self::new(NonNull::from(space)))?;
        object::allocate(space, port)
    }

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

    /// Takes a reference to it.
    pub(super) fn reference(&self) {
        self.references.fetch_add(1, Ordering::Relaxed);
    }

    /// Drops a reference to it, which is not its last.
    ///
    /// # Panics
    ///
    /// On its last reference: not yet.
    fn release(&self) {
        // Only the last reference's drop frees it.
        assert!(
            self.references
                .try_update(Ordering::Release, Ordering::Relaxed, |references| {
                    (references > 1).then(|| references - 1)
                })
                .is_ok(),
            "ip_release: the last reference, not yet"
        );
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

    /// Releases a send right for it, which the caller holds, with its
    /// reference, as `ipc_port_release_send` does.
    ///
    /// # Panics
    ///
    /// If it has no send right. If it is dead, or the right is its last
    /// send right: not yet. As `release`.
    pub fn release_send(&self) {
        let mut state = self.state.lock();
        self.release();
        assert!(state.active, "ipc_port_release_send: a dead port, not yet");
        state.send_rights = state
            .send_rights
            .checked_sub(1)
            .expect("ipc_port_release_send: no send right");
        assert!(
            state.send_rights != 0,
            "ipc_port_release_send: no senders left, not yet"
        );
        drop(state);
    }

    /// Puts a send right for it, which the caller holds, in `space`, as
    /// `ipc_port_copyout_send` does; gives its name there.
    ///
    /// # Panics
    ///
    /// If the copy-out fails: not yet; as `object::copy_out`.
    pub fn copy_out_send(&self, space: &Space) -> PortName {
        object::copy_out(space, self, MessageType::PortSend)
            .expect("ipc_port_copyout_send: a failed copy-out, not yet")
    }
}

/// A port's name in a space, as Mach's `mach_port_name_t`.
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct PortName(u32);

impl PortName {
    /// `MACH_PORT_NAME_NULL`: no port.
    pub(super) const NULL: Self = Self(0);
    /// `MACH_PORT_NAME_DEAD`: a dead port.
    const DEAD: Self = Self(u32::MAX);

    /// Whether it names a port, as `MACH_PORT_NAME_VALID`: neither null nor
    /// dead.
    pub(super) fn is_valid(self) -> bool {
        self != Self::NULL && self != Self::DEAD
    }

    /// Its key in a space's map.
    pub(super) fn key(self) -> u64 {
        u64::from(self.0)
    }

    /// The name of `key` in a space's map.
    ///
    /// # Panics
    ///
    /// If `key` is past 32 bits.
    pub(super) fn from_key(key: u64) -> Self {
        Self(u32::try_from(key).unwrap())
    }
}

impl From<u32> for PortName {
    fn from(name: u32) -> Self {
        Self(name)
    }
}

impl From<PortName> for u32 {
    fn from(name: PortName) -> Self {
        name.0
    }
}

/// What a port the kernel receives on stands for; only the kinds used here.
#[derive(Clone, Copy)]
pub enum KernelObject {
    /// A memory object's name port, which names it to `vm_region`.
    PagerName(#[expect(dead_code, reason = "vm_object_lookup_name reads it")] NonNull<Object>),
    /// The host's port.
    Host(&'static Host),
    /// The host's privileged port.
    HostPrivileged(&'static Host),
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
