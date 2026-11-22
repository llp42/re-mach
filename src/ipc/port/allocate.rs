// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};

use intrusive_collections::LinkedList;

use super::port::{Port, Receiver};
use super::port_name::PortName;
use super::state::PortState;
use crate::error::Error;
use crate::ipc::kernel_message;
use crate::ipc::object;
use crate::ipc::space::{self, Space};
use crate::reference::{Count, Reference};
use crate::slab::{Cache, CacheBox};
use crate::sync::Spinlock;
use crate::thread::thread::IpcQueueAdapter;
impl Port {
    /// A new live port that `receiver` receives on, with one reference and
    /// no rights out, as `ipc_port_init` sets it up.
    const fn new(receiver: NonNull<Space>) -> Self {
        Self {
            references: Count::new(1),
            state: Spinlock::new(PortState {
                active: true,
                receiver: Receiver::Space(receiver),
                receiver_name: PortName::NULL,
                send_rights: 0,
                send_once_rights: 0,
                make_send_count: 0,
                kernel_object: None,
                set: None,
                sequence_number: 0,
                messages: LinkedList::new(kernel_message::QueueAdapter::NEW),
                message_count: 0,
                // MACH_PORT_QLIMIT_DEFAULT.
                queue_limit: 5,
                receivers: LinkedList::new(IpcQueueAdapter::NEW),
                protected_payload: None,
                orphaned: false,
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

    /// A new port that receives kernel replies, with one reference, as
    /// `ipc_port_alloc_reply` makes it.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no port can be allocated.
    pub fn allocate_reply() -> Result<CacheBox<Self>, Error> {
        let mut port = CACHE.allocate(Self::new(NonNull::from(&space::REPLY_SPACE)))?;
        // Its address's low 32 bits, as GNU Mach names special ports.
        let address = ptr::from_ref(&*port).addr() & 0xffff_ffff;
        port.state.get_mut().receiver_name = PortName::from(u32::try_from(address).unwrap());
        Ok(port)
    }

    /// A new port that `space` receives on, under a free name there, its one
    /// reference the receive right's; gives the name.
    ///
    /// # Errors
    ///
    /// As `Space::insert_object`.
    ///
    /// # Panics
    ///
    /// As `Space::insert_object`.
    pub fn allocate(space: &Space) -> Result<PortName, Error> {
        let port = CACHE.allocate(Self::new(NonNull::from(space)))?;
        space.insert_object(
            port,
            // As ipc_port_init: its name there.
            |port, name| port.state.get_mut().receiver_name = name,
            |port| object::Object::Port(Reference::from(port)),
        )
    }

    /// A new port that `space` receives on, under `name` there, its one
    /// reference the receive right's, as `ipc_port_alloc_name` makes it.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no port can be allocated; as
    /// `Space::insert_object_as`.
    ///
    /// # Panics
    ///
    /// As `Space::insert_object_as`.
    pub(in crate::ipc) fn allocate_name(space: &Space, name: PortName) -> Result<(), Error> {
        let mut port = CACHE.allocate(Self::new(NonNull::from(space)))?;
        // As ipc_port_init: its name there.
        port.state.get_mut().receiver_name = name;
        space.insert_object_as(port, name, |port| {
            object::Object::Port(Reference::from(port))
        })
    }

    /// Destroys the port, alive and the kernel's, as `ipc_port_dealloc_kernel`
    /// does: no longer the kernel's, its receiver cleared, then dead. The
    /// caller then drops the reference GNU Mach's call consumes.
    ///
    /// # Panics
    ///
    /// If it is dead or not the kernel's. As `PortState::clear_receiver`
    /// and `Port::destroy`.
    pub fn deallocate_kernel(&self) {
        let mut state = self.state.lock();
        assert!(state.active, "ipc_port_dealloc_special: a dead port");
        assert!(
            state.receiver_name != PortName::NULL
                && state.receiver == Receiver::Space(NonNull::from(&space::KERNEL_SPACE)),
            "ipc_port_dealloc_special: not the kernel's"
        );
        // Its receiver goes; `destroy` marks it dead.
        state.receiver_name = PortName::NULL;
        state.clear_receiver();
        self.destroy(state);
    }
}

/// The cache ports come from.
pub(in crate::ipc) static CACHE: Cache<Port> = Cache::new("ipc_port");
