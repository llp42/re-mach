// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Spaces: a task's names for ports and port sets.

use core::ptr::NonNull;
use core::sync::atomic::AtomicU32;

use super::entry::{Entry, ZERO_ENTRY};
use super::port::Port;
use crate::kernel::Error;
use crate::kernel::radix_tree::RadixTree;
use crate::kernel::slab::{Cache, CacheBox};

/// A space: the names a task gives ports and port sets.
#[expect(dead_code, reason = "the space and right calls use them")]
pub struct Space {
    /// How many references it has.
    references: AtomicU32,
    /// Whether it is in use; special spaces never are.
    active: bool,
    /// Its entries, by name.
    map: RadixTree<Entry>,
    /// Its names for ports, by port.
    reverse_map: RadixTree<Port>,
    /// How many names it holds.
    size: u32,
}

impl Space {
    /// A special space: one reference, never in use.
    const fn special() -> Self {
        Self {
            references: AtomicU32::new(1),
            active: false,
            map: RadixTree::new(),
            reverse_map: RadixTree::new(),
            size: 0,
        }
    }

    /// A new space, in use, with two references; name 0 is reserved.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if no space can be allocated.
    pub fn create() -> Result<CacheBox<Self>, Error> {
        let mut map = RadixTree::new();
        map.insert(0, NonNull::from(&ZERO_ENTRY))
            .map_err(|_| Error::ResourceShortage)?;
        CACHE.allocate(Self {
            references: AtomicU32::new(2),
            active: true,
            map,
            reverse_map: RadixTree::new(),
            size: 1,
        })
    }
}

/// The cache spaces come from.
pub(super) static CACHE: Cache<Space> = Cache::new("ipc_space");

/// The space of the ports the kernel receives; never in use.
pub static KERNEL_SPACE: Space = Space::special();

/// The space that marks ports receiving kernel replies; never in use.
#[expect(dead_code, reason = "kernel replies mark their ports with it")]
pub static REPLY_SPACE: Space = Space::special();
