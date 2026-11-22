// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::{self, ManuallyDrop};
use core::ptr::NonNull;

use super::port::Port;
use crate::device::Device;
use crate::device::pager::Pager;
use crate::host::Host;
use crate::ipc::space::Space;
use crate::processor::processor::Processor;
use crate::processor::set::ProcessorSet;
use crate::reference::Reference;
use crate::task::task::Task;
use crate::thread::thread::Thread;
use crate::vm::map::map::Map;
use crate::vm::object::{Flags, Object};
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

impl Port {
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
