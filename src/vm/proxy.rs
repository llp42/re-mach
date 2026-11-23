// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Proxy memory objects: ports that map part of a memory object, allowing
//! at most a given protection.

use core::ops::Range;
use core::ptr::NonNull;

use super::Protection;
use crate::error::Error;
use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::reference::Reference;
use crate::slab::{Cache, CacheBox};

/// A proxy memory object; it holds a send right for the object it maps.
pub struct Proxy {
    /// The object it maps part of.
    object: Reference<Port>,
    /// The most a mapping of it may allow.
    max_protection: Protection,
    /// The part of the memory object it maps, by offset.
    range: Range<usize>,
}

/// The cache proxies come from.
static CACHE: Cache<Proxy> = Cache::new("memory_object_proxy");

/// Sets up proxy memory objects, on the boot CPU.
pub fn init() {
    CACHE.register();
}

impl Proxy {
    /// Makes a proxy for `range` of `object`, which it keeps, allowing at
    /// most `max_protection`, as `memory_object_create_proxy` makes one;
    /// gives a send right for its port, whose replies copy it out.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no proxy or port can be allocated.
    ///
    /// # Panics
    ///
    /// If `range` ends before it starts.
    pub fn create(
        object: Reference<Port>,
        max_protection: Protection,
        range: Range<usize>,
    ) -> Result<Reference<Port>, Error> {
        assert!(
            range.start <= range.end,
            "memory_object_create_proxy: KERN_INVALID_ARGUMENT"
        );
        let proxy = CACHE.allocate(Self {
            object,
            max_protection,
            range,
        })?;
        let port = Port::allocate_kernel()?;
        port.set_kernel_object(KernelObject::PagerProxy(NonNull::from(&*proxy)));
        // The port keeps the proxy; nothing frees it yet.
        CacheBox::into_raw(proxy);
        let port = Reference::from(port);
        Ok(Port::make_send(&port))
    }

    /// The object it maps part of.
    pub const fn object(&self) -> &Reference<Port> {
        &self.object
    }

    /// The most a mapping of it may allow.
    pub const fn max_protection(&self) -> Protection {
        self.max_protection
    }

    /// The part of the object it maps, by offset.
    pub fn range(&self) -> Range<usize> {
        self.range.clone()
    }
}
