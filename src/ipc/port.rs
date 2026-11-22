// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Ports.

use core::mem::{self, ManuallyDrop};
use core::ops::Deref;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicU32, Ordering};

use intrusive_collections::LinkedList;

use super::entry::Rights;
use super::port_set::PortSet;
use super::space::{self, Space};
use super::{MessageType, kernel_message, object};
use crate::device::Device;
use crate::device::pager::Pager;
use crate::error::Error;
use crate::host::Host;
use crate::processor::{Processor, ProcessorSet};
use crate::reference::{Count, Counted, Reference};
use crate::slab::{Cache, CacheBox};
use crate::sync::{Spinlock, SpinlockGuard};
use crate::task::task::Task;
use crate::thread::thread::{IpcQueueAdapter, Thread};
use crate::vm::map::Map;
use crate::vm::object::{Flags, Object};

/// A port: its reference count, and its state under its lock.
pub struct Port {
    /// How many references it has.
    references: Count,
    /// Its state.
    pub(super) state: Spinlock<PortState>,
}

/// A port's state under its lock; so far only whether it is alive, its
/// receiver and its name there, its send and send-once rights, what it
/// stands for, its set, its sequence number, its message queue, its count
/// and limit, its protected payload, and whether it is an orphaned reply
/// port.
pub(super) struct PortState {
    /// Whether it is alive.
    pub(super) active: bool,
    /// Who holds its receive right.
    pub(super) receiver: Receiver,
    /// Its receive right's name in that space.
    pub(super) receiver_name: PortName,
    /// How many send rights for it there are.
    pub(super) send_rights: u32,
    /// How many send-once rights for it there are.
    pub(super) send_once_rights: u32,
    /// How many send rights have been made from its receive right.
    pub(super) make_send_count: u32,
    /// What it stands for, when the kernel receives on it.
    kernel_object: Option<KernelObject>,
    /// The set it is in, if any.
    pub(super) set: Option<NonNull<PortSet>>,
    /// The sequence number of the next message received from it.
    pub(super) sequence_number: u32,
    /// The messages queued on it, oldest first.
    pub(super) messages: LinkedList<kernel_message::QueueAdapter>,
    /// How many messages are queued on it.
    pub(super) message_count: u32,
    /// How many messages it queues before senders wait.
    pub(super) queue_limit: u32,
    /// The threads waiting to receive from it, latest first.
    pub(super) receivers: LinkedList<IpcQueueAdapter>,
    /// What its received messages carry in place of its name, if set.
    pub(super) protected_payload: Option<u64>,
    /// Whether, as a reply port, its thread ended: it is destroyed once every
    /// send-once right for it sits in its own queue.
    pub(super) orphaned: bool,
}

// SAFETY: its pointers are to spaces, sets, kernel objects, queued messages
// and waiting threads, which any CPU may use; its lock serializes changes
// to them.
unsafe impl Send for PortState {}

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
    pub(super) fn allocate_name(space: &Space, name: PortName) -> Result<(), Error> {
        let mut port = CACHE.allocate(Self::new(NonNull::from(space)))?;
        // As ipc_port_init: its name there.
        port.state.get_mut().receiver_name = name;
        space.insert_object_as(port, name, |port| {
            object::Object::Port(Reference::from(port))
        })
    }

    /// Makes it stand for `object`.
    pub fn set_kernel_object(&self, object: KernelObject) {
        self.state.lock().kernel_object = Some(object);
    }

    /// Makes it stand for `object`, or nothing for none; gives what it stood
    /// for, alive or not.
    pub fn replace_kernel_object(&self, object: Option<KernelObject>) -> Option<KernelObject> {
        mem::replace(&mut self.state.lock().kernel_object, object)
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

    /// Runs `action` on what it stands for, while it is alive, under its
    /// lock; gives what `action` gives.
    pub fn with_kernel_object<R>(&self, action: impl FnOnce(Option<&KernelObject>) -> R) -> R {
        let state = self.state.lock();
        let result = action(state.kernel_object.as_ref().filter(|_| state.active));
        drop(state);
        result
    }

    /// Makes a send right for `port` from its receive right, whose
    /// reference the caller lends; gives the right's reference.
    pub fn make_send(port: &Reference<Self>) -> Reference<Self> {
        let mut state = port.state.lock();
        state.make_send_count += 1;
        state.send_rights += 1;
        drop(state);
        port.clone()
    }

    /// Gives up `port`, the reply port of a thread that ends, with the
    /// thread's reference, which drops (SC-D2): with send-once rights out of
    /// its queue, it is orphaned, destroyed once they all sit there.
    ///
    /// # Panics
    ///
    /// If it is dead, not a reply port, or has send rights. With no
    /// send-once right out of its queue: not yet.
    pub fn orphan_reply(port: Reference<Self>) {
        let mut state = port.state.lock();
        assert!(
            state.active
                && state.receiver == Receiver::Space(NonNull::from(&space::REPLY_SPACE))
                && state.send_rights == 0,
            "orphan_reply: not a live reply port, or one with send rights"
        );
        assert!(
            !state.send_once_rights_queued(),
            "orphan_reply: ipc_port_dealloc_reply, not yet"
        );
        state.orphaned = true;
        drop(state);
        drop(port);
    }

    /// Destroys the port, which receives kernel replies, as
    /// `ipc_port_dealloc_reply` does: its receiver goes, then it is
    /// destroyed. The caller then drops the reference GNU Mach's call
    /// consumes.
    ///
    /// # Panics
    ///
    /// If it is dead or not a reply port. As `destroy`.
    pub(super) fn deallocate_reply(&self) {
        let mut state = self.state.lock();
        assert!(state.active, "ipc_port_dealloc_special: a dead port");
        assert!(
            state.receiver_name != PortName::NULL
                && state.receiver == Receiver::Space(NonNull::from(&space::REPLY_SPACE)),
            "ipc_port_dealloc_special: not the reply space's"
        );
        // Its receiver goes; `destroy` marks it dead.
        state.receiver_name = PortName::NULL;
        state.clear_receiver();
        self.destroy(state);
    }

    /// Makes a send-once right for `port` from its receive right, whose
    /// reference the caller lends, as `ipc_port_make_sonce` does; gives the
    /// right's reference.
    ///
    /// # Panics
    ///
    /// If `port` is dead: not yet.
    pub fn make_send_once(port: &Reference<Self>) -> Reference<Self> {
        let mut state = port.state.lock();
        assert!(state.active, "ipc_port_make_sonce: a dead port, not yet");
        state.send_once_rights += 1;
        drop(state);
        port.clone()
    }

    /// Makes a send right for `port` from another, whose reference the
    /// caller lends, as `ipc_port_copy_send` does; gives the new right's
    /// reference.
    ///
    /// # Panics
    ///
    /// If `port` has no send right. If it is dead: not yet.
    pub fn copy_send(port: &Reference<Self>) -> Reference<Self> {
        let mut state = port.state.lock();
        assert!(state.active, "ipc_port_copy_send: a dead port, not yet");
        assert!(state.send_rights > 0, "ipc_port_copy_send: no send right");
        let send = port.clone();
        state.send_rights += 1;
        drop(state);
        send
    }

    /// Makes a send right for `port` from another, whose reference the
    /// caller lends, as `ipc_port_copy_send` does: gives the new right's
    /// reference, or none for a dead port.
    ///
    /// # Panics
    ///
    /// If `port` is alive and has no send right.
    pub fn try_copy_send(port: &Reference<Self>) -> Option<Reference<Self>> {
        let mut state = port.state.lock();
        let send = state.active.then(|| {
            assert!(state.send_rights > 0, "ipc_port_copy_send: no send right");
            state.send_rights += 1;
            port.clone()
        });
        drop(state);
        send
    }

    /// Releases a send right for `port`, which the caller gives with its
    /// reference, as `ipc_port_release_send` does: a dead port keeps no
    /// count.
    ///
    /// # Panics
    ///
    /// If `port` is alive and has no send right.
    pub fn release_send(port: Reference<Self>) {
        let mut state = port.state.lock();
        if state.active {
            state.send_rights = state
                .send_rights
                .checked_sub(1)
                .expect("ipc_port_release_send: no send right");
            // With no no-senders request, the last sender's release does no
            // more.
        }
        drop(state);
        drop(port);
    }

    /// Releases a send-once right for `port`, which the caller gives with
    /// its reference, as `ipc_port_release_sonce` does.
    ///
    /// # Panics
    ///
    /// If `port` has no send-once right.
    pub fn release_send_once(port: Reference<Self>) {
        let mut state = port.state.lock();
        state.send_once_rights = state
            .send_once_rights
            .checked_sub(1)
            .expect("ipc_port_release_sonce: no send-once right");
        drop(state);
        drop(port);
    }

    /// Puts a send right for `port`, which the caller gives with its
    /// reference, in `space`, as `ipc_port_copyout_send` does; gives its
    /// name there.
    ///
    /// # Panics
    ///
    /// If the copy-out fails: not yet; as `Space::copy_out`.
    pub fn copy_out_send(port: Reference<Self>, space: &Space) -> PortName {
        space
            .copy_out(port, MessageType::PortSend)
            .unwrap_or_else(|(error, _)| {
                panic!("ipc_port_copyout_send: a failed copy-out, not yet: {error:?}")
            })
    }

    /// Destroys the port, alive, cleared of its receiver and of locked state
    /// `state`, as `ipc_port_destroy` does: it is dead and unlocked, and its
    /// queued messages are destroyed. The caller then drops the reference
    /// GNU Mach's call consumes.
    ///
    /// # Panics
    ///
    /// If it is dead, in a set, its make-send count or sequence number is
    /// not 0, a thread waits to receive from it, or a queued message is for
    /// another port. If it has a kernel object: not yet. As
    /// `KernelMessage::destroy`.
    pub(super) fn destroy(&self, mut state: SpinlockGuard<'_, PortState>) {
        assert!(state.active, "ipc_port_destroy: a dead port");
        assert!(
            state.set.is_none() && state.make_send_count == 0 && state.sequence_number == 0,
            "ipc_port_destroy: a receiver not cleared"
        );
        // Port-destroyed, no-senders and dead-name requests and blocked
        // senders come with the calls that make them, and their checks too.
        state.active = false;
        state.receiver = Receiver::Dead(timestamp());
        drop(state);
        // The queue's lock is the port's.
        let mut state = self.state.lock();
        assert!(
            state.receivers.is_empty(),
            "ipc_port_destroy: a thread receiving"
        );
        // Each queued message is destroyed with the port unlocked, its
        // destination's reference released first.
        while let Some(mut message) = kernel_message::KernelMessage::dequeue(&mut state.messages) {
            state.message_count -= 1;
            drop(state);
            let header = message.header_mut();
            assert!(
                header.remote_port == ptr::from_ref(self).addr(),
                "ipc_port_destroy: a message for another port"
            );
            // SAFETY: a queued message's destination slot holds a reference to
            // its port, this one, given up by `into_raw`, which the handle
            // takes; the caller's reference keeps the port alive after it.
            drop(unsafe { Reference::from_raw(NonNull::from(self)) });
            header.remote_port = 0;
            message.destroy();
            state = self.state.lock();
        }
        assert!(
            state.kernel_object.is_none(),
            "ipc_port_destroy: ipc_kobject_destroy, not yet"
        );
        drop(state);
        // ipc_target_terminate does nothing.
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

impl Port {
    /// The host it stands for while alive, if it is one of a host's ports,
    /// as `convert_port_to_host` finds it.
    pub fn as_host(&self) -> Option<&'static Host> {
        match self.kernel_object() {
            Some(KernelObject::Host(host) | KernelObject::HostPrivileged(host)) => Some(host),
            _ => None,
        }
    }

    /// The host it stands for while alive, if it is a host's privileged
    /// port, as `convert_port_to_host_priv` finds it.
    pub fn as_privileged_host(&self) -> Option<&'static Host> {
        match self.kernel_object() {
            Some(KernelObject::HostPrivileged(host)) => Some(host),
            _ => None,
        }
    }

    /// The set it stands for, if it is a set's port, not its name port,
    /// with a reference to it taken under the port's lock, as
    /// `convert_port_to_pset` gives it.
    pub fn to_processor_set(&self) -> Option<Reference<ProcessorSet>> {
        self.with_kernel_object(|object| match object {
            Some(KernelObject::ProcessorSet(set)) => {
                // SAFETY: a set's ports keep a reference to it while they stand
                // for it, which this handle borrows and never drops.
                let set = ManuallyDrop::new(unsafe { Reference::from_raw(*set) });
                Some(Reference::clone(&set))
            }
            _ => None,
        })
    }

    /// The set it stands for, if it is a set's port or name port, with a
    /// reference to it taken under the port's lock, as
    /// `convert_port_to_pset_name` gives it.
    pub fn to_processor_set_name(&self) -> Option<Reference<ProcessorSet>> {
        self.with_kernel_object(|object| match object {
            Some(KernelObject::ProcessorSet(set) | KernelObject::ProcessorSetName(set)) => {
                // SAFETY: a set's ports keep a reference to it while they stand
                // for it, which this handle borrows and never drops.
                let set = ManuallyDrop::new(unsafe { Reference::from_raw(*set) });
                Some(Reference::clone(&set))
            }
            _ => None,
        })
    }

    /// The task it stands for, with a reference to it taken under the port's
    /// lock, as `convert_port_to_task` gives it.
    pub fn to_task(&self) -> Option<Reference<Task>> {
        self.with_kernel_object(|object| match object {
            // SAFETY: a task outlives its port's standing for it.
            Some(KernelObject::Task(task)) => unsafe { Reference::upgrade(*task) },
            _ => None,
        })
    }

    /// The space of the task it stands for, with a reference to it taken
    /// under the port's lock, as `convert_port_to_space` gives it.
    pub fn task_space(&self) -> Option<Reference<Space>> {
        self.with_kernel_object(|object| match object {
            // SAFETY: a task outlives its port's standing for it.
            Some(KernelObject::Task(task)) => Some(unsafe { task.as_ref() }.space.clone()),
            _ => None,
        })
    }

    /// The map of the task it stands for, with a reference to it taken
    /// under the port's lock, as `convert_port_to_map` gives it.
    pub fn task_map(&self) -> Option<Reference<Map>> {
        self.with_kernel_object(|object| match object {
            // SAFETY: a task outlives its port's standing for it.
            Some(KernelObject::Task(task)) => Some(unsafe { task.as_ref() }.map.clone()),
            _ => None,
        })
    }

    /// The thread it stands for, with a reference to it taken under the port's
    /// lock, as `convert_port_to_thread` gives it.
    pub fn to_thread(&self) -> Option<Reference<Thread>> {
        self.with_kernel_object(|object| match object {
            // SAFETY: a thread outlives its port's standing for it.
            Some(KernelObject::Thread(thread)) => unsafe { Reference::upgrade(*thread) },
            _ => None,
        })
    }

    /// The device it stands for, with a reference to it taken under the port's
    /// lock, as `dev_port_lookup`; none for a dead port or one standing
    /// for no device.
    pub fn to_device(&self) -> Option<Reference<Device>> {
        self.with_kernel_object(|object| match object {
            Some(&KernelObject::Device(device)) => {
                // SAFETY: the port's reference keeps it alive; the handle borrows
                // it and never drops.
                let held = ManuallyDrop::new(unsafe { Reference::from_raw(device) });
                Some(Reference::clone(&held))
            }
            _ => None,
        })
    }

    /// The object whose request port it is, with a reference taken, as
    /// `vm_object_lookup` gives it; none if it is no request port, or its last
    /// reference dropped and it is being terminated, which takes it off the
    /// port next.
    ///
    /// # Panics
    ///
    /// If the object is dead.
    pub(crate) fn to_vm_object(&self) -> Option<Reference<Object>> {
        let object = self.with_kernel_object(|kernel_object| match kernel_object {
            // SAFETY: the port stands for its object until `terminate` clears
            // it, under the port's lock, before freeing the object.
            Some(&KernelObject::PagingRequest(object)) => unsafe { Reference::upgrade(object) },
            _ => None,
        })?;
        // Its reference keeps `terminate` off.
        assert!(
            object.lock().flags.contains(Flags::ALIVE),
            "vm_object_lookup: a dead object"
        );
        Some(object)
    }
}

impl PortState {
    /// Whether every send-once right for it sits in its own queue, one per
    /// queued message whose destination is a send-once right; a right in
    /// flight counts as out, so this errs toward false. Under its lock.
    pub(super) fn send_once_rights_queued(&self) -> bool {
        let queued = self
            .messages
            .iter()
            .filter(|buffer| {
                matches!(
                    buffer.header().bits.remote(),
                    Some(MessageType::PortSendOnce)
                )
            })
            .count();
        usize::try_from(self.send_once_rights).unwrap() == queued
    }

    /// Readies its port's receive right to be destroyed, its port alive, as
    /// `ipc_port_clear_receiver` does.
    ///
    /// # Panics
    ///
    /// If its port is dead. If it is in a set, or a thread waits to receive
    /// from it: not yet.
    pub(super) fn clear_receiver(&mut self) {
        assert!(self.active, "ipc_port_clear_receiver: a dead port");
        assert!(
            self.set.is_none(),
            "ipc_port_clear_receiver: ipc_pset_remove, not yet"
        );
        // As ipc_mqueue_changed, under the queue's lock, the port's.
        assert!(
            self.receivers.is_empty(),
            "ipc_mqueue_changed: thread_go, not yet"
        );
        // As ipc_port_set_mscount.
        self.make_send_count = 0;
        self.sequence_number = 0;
    }
}

// SAFETY: its count is its own, and every port is a box of `CACHE`, which
// `free` takes back.
unsafe impl Counted for Port {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `port`, as `ipc_port_release` does at zero: `io_check_unlock`
    /// frees it to its cache.
    unsafe fn free(port: NonNull<Self>) {
        // SAFETY: every port is a `CACHE` box that `From<CacheBox<Port>>`
        // gave up with `into_raw`, and nothing uses this one after.
        drop(unsafe { CacheBox::from_raw(&CACHE, port) });
    }
}

impl From<CacheBox<Port>> for Reference<Port> {
    /// The handle of the reference `port`'s box holds.
    fn from(port: CacheBox<Port>) -> Self {
        // SAFETY: a port's box holds its first reference, given up here.
        unsafe { Self::from_raw(CacheBox::into_raw(port)) }
    }
}

/// A send right for a port, released as it drops.
pub struct SendRight(ManuallyDrop<Reference<Port>>);

impl SendRight {
    /// The handle of a send right for `port`, which the caller gives with its
    /// reference.
    pub const fn new(port: Reference<Port>) -> Self {
        Self(ManuallyDrop::new(port))
    }

    /// Its port's reference, which takes the right with it, unreleased.
    pub fn into_reference(self) -> Reference<Port> {
        let mut right = ManuallyDrop::new(self);
        // SAFETY: `right` never drops, and its field is not used after.
        unsafe { ManuallyDrop::take(&mut right.0) }
    }
}

impl AsRef<Reference<Port>> for SendRight {
    fn as_ref(&self) -> &Reference<Port> {
        &self.0
    }
}

impl Deref for SendRight {
    type Target = Port;

    fn deref(&self) -> &Port {
        &self.0
    }
}

impl Drop for SendRight {
    /// Releases the right, as `ipc_port_release_send` does.
    fn drop(&mut self) {
        // SAFETY: drop runs once, and the field is not used after.
        Port::release_send(unsafe { ManuallyDrop::take(&mut self.0) });
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

bitflags::bitflags! {
    /// The rights a name holds, Mach's `mach_port_type_t`; its bits come
    /// from `Rights`.
    #[repr(transparent)]
    #[derive(Clone, Copy)]
    pub struct PortType: u32 {}
}

impl From<Rights> for PortType {
    /// The type bits of `rights`, as `IE_BITS_TYPE` takes them.
    fn from(rights: Rights) -> Self {
        Self::from_bits_retain(u32::from(rights.bits()) << 16)
    }
}

/// A kind of right, Mach's `mach_port_right_t`; only the kinds used here.
pub(super) enum Right {
    /// `MACH_PORT_RIGHT_RECEIVE`.
    Receive,
    /// `MACH_PORT_RIGHT_PORT_SET`.
    PortSet,
    /// `MACH_PORT_RIGHT_DEAD_NAME`.
    DeadName,
}

impl TryFrom<u32> for Right {
    type Error = u32;

    /// The kind of Mach's number `number`, if it is one used here.
    fn try_from(number: u32) -> Result<Self, u32> {
        match number {
            1 => Ok(Self::Receive),
            3 => Ok(Self::PortSet),
            4 => Ok(Self::DeadName),
            _ => Err(number),
        }
    }
}

/// A logical time that orders port deaths, Mach's `ipc_port_timestamp_t`.
#[derive(Clone, Copy, PartialEq)]
pub(super) struct Timestamp(u32);

/// Who holds a port's receive right, Mach's `data` union (`ip_receiver`,
/// `ip_timestamp`); only the kinds used here.
#[derive(Clone, Copy, PartialEq)]
pub(super) enum Receiver {
    /// A space.
    Space(NonNull<Space>),
    /// No one: the port died at that time, Mach's `ip_timestamp`.
    Dead(Timestamp),
}

/// The next timestamp, in place of `ipc_port_timestamp_lock` and its data.
static TIMESTAMP: AtomicU32 = AtomicU32::new(0);

/// A new timestamp, as `ipc_port_timestamp` gives it.
pub(super) fn timestamp() -> Timestamp {
    Timestamp(TIMESTAMP.fetch_add(1, Ordering::Relaxed))
}

/// What a port the kernel receives on stands for; only the kinds used here.
#[derive(Clone, Copy)]
pub enum KernelObject {
    /// A device's port, holding a reference to it.
    Device(NonNull<Device>),
    /// A device pager's port, the memory object of a device's memory, until
    /// an object is made for it.
    DevicePager(NonNull<Pager>),
    /// A memory object's port, once the kernel made an object for it.
    Pager(#[expect(dead_code, reason = "vm_object_enter's hit reads it")] NonNull<Object>),
    /// A memory object's request port, which its pager calls the kernel on.
    PagingRequest(NonNull<Object>),
    /// A memory object's name port, which names it to `vm_region`.
    PagerName(#[expect(dead_code, reason = "vm_object_lookup_name reads it")] NonNull<Object>),
    /// The host's port.
    Host(&'static Host),
    /// The host's privileged port.
    HostPrivileged(&'static Host),
    /// A processor set's port.
    ProcessorSet(NonNull<ProcessorSet>),
    /// A processor set's name port.
    ProcessorSetName(NonNull<ProcessorSet>),
    /// A processor's port.
    Processor(
        #[expect(dead_code, reason = "convert_port_to_processor reads it")] &'static Processor,
    ),
    /// A processor's name port.
    ProcessorName(#[expect(dead_code, reason = "processor_info reads it")] &'static Processor),
    /// A task's port.
    Task(NonNull<Task>),
    /// A thread's port.
    Thread(NonNull<Thread>),
}

/// The cache ports come from.
pub(super) static CACHE: Cache<Port> = Cache::new("ipc_port");
