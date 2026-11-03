// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Ports.

use core::ptr::NonNull;

use super::space::{self, Space};
use crate::kernel::Error;
use crate::kernel::host::Host;
use crate::kernel::processor::{Processor, ProcessorSet};
use crate::kernel::slab::{Cache, CacheBox};
use crate::vm::object::Object;

/// A port; so far only its reference count, whether it is alive, its
/// receiver and what it stands for.
#[expect(dead_code, reason = "the port calls and ipc_kobject_server read them")]
pub struct Port {
    /// How many references it has.
    references: u32,
    /// Whether it is alive.
    active: bool,
    /// The space that holds its receive right.
    receiver: NonNull<Space>,
    /// What it stands for, when the kernel receives on it.
    kernel_object: Option<KernelObject>,
}

impl Port {
    /// Makes it stand for `object`; it isn't shared yet, so no lock.
    pub const fn set_kernel_object(&mut self, object: KernelObject) {
        self.kernel_object = Some(object);
    }
}

/// What a port the kernel receives on stands for; only the kinds used here.
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
        references: 1,
        active: true,
        receiver: NonNull::from(&space::KERNEL_SPACE),
        kernel_object: None,
    })
}
