// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ops::{DerefMut, Range};
use core::ptr::NonNull;

use intrusive_collections::RBTree;

use super::contents::Contents;
use super::copy::COPY_CACHE;
use super::entry::{ENTRY_CACHE, EntryAdapter, EntryReservation, GapAdapter};
use crate::arch::PhysMap;
use crate::error::Error;
use crate::reference::{Count, Counted};
use crate::slab::{Cache, CacheBox};
use crate::sync::SleepLock;
use crate::vm::VirtualAddress;
use crate::vm::page;
use crate::vm::phys_map::MachinePhysMap as _;

/// An address map: ranges of virtual memory and what they map; so far only
/// what a new map sets.
pub struct Map {
    /// The addresses it may map.
    pub(super) bounds: Range<VirtualAddress>,
    /// The physical map its mappings go into.
    #[expect(clippy::struct_field_names, reason = "named for its type, PhysMap")]
    pub(super) phys_map: &'static PhysMap,
    /// How many references it has.
    references: Count,
    /// What changes as it maps and unmaps, behind its lock.
    pub(in crate::vm) contents: SleepLock<Contents>,
}

impl Map {
    /// An empty map of `bounds` over `phys_map`, with one reference and no
    /// size limit.
    pub fn new(phys_map: &'static PhysMap, bounds: Range<VirtualAddress>) -> Self {
        let leading_gap = bounds.end - bounds.start;
        Self {
            bounds,
            phys_map,
            references: Count::new(1),
            contents: SleepLock::new(Contents {
                entries: RBTree::new(EntryAdapter::NEW),
                gaps: RBTree::new(GapAdapter::NEW),
                gap_inserts: 0,
                leading_gap,
                first_free: None,
                size: 0,
                size_wired: 0,
                size_none: 0,
                wiring_required: false,
                size_limit: usize::MAX,
                max_size_limit: usize::MAX,
                timestamp: 0,
            }),
        }
    }
    /// The physical map its mappings go into.
    pub const fn phys_map(&self) -> &'static PhysMap {
        self.phys_map
    }
    /// How much it maps, in bytes, read under its read lock.
    pub fn size(&self) -> usize {
        self.contents.read().size
    }

    /// How many bytes are wired, as `vm_map`'s `size_wired`.
    pub fn size_wired(&self) -> usize {
        self.contents.read().size_wired
    }

    /// A new empty map of `bounds` over `phys_map`, a user task's, with one
    /// reference, its size limited to the memory's.
    ///
    /// # Errors
    ///
    /// `ResourceShortage` if it can't be allocated.
    pub fn create(
        phys_map: &'static PhysMap,
        bounds: Range<VirtualAddress>,
    ) -> Result<CacheBox<Self>, Error> {
        let size = page::load::memory_size();
        let mut map = Self::new(phys_map, bounds);
        let contents = map.contents.get_mut();
        contents.size_limit = size;
        contents.max_size_limit = size;
        MAP_CACHE.allocate(map)
    }
    /// Takes the size limits of `source`, read under its read lock, as
    /// `task_create` does with `vm_map_copy_limits`.
    pub fn copy_limits(&mut self, source: &Self) {
        let limits = source.contents.read();
        self.contents.get_mut().copy_limits(&limits);
        drop(limits);
    }
    /// Takes its read lock if it is still at `version`, as a lookup found
    /// it; it stays locked until what is returned drops.
    pub fn verify(&self, version: u32) -> Option<impl Sized + '_> {
        let contents = self.contents.read();
        (contents.timestamp == version).then_some(contents)
    }

    /// Takes its write lock, moving its version on.
    pub(in crate::vm) fn lock(&self) -> impl DerefMut<Target = Contents> + '_ {
        let mut contents = self.contents.write();
        contents.timestamp = contents.timestamp.wrapping_add(1);
        contents
    }
}

// SAFETY: its count is its own, and every map but the kernel's maps, whose
// own references never drop, is a box of `MAP_CACHE`, which `free` takes
// back.
unsafe impl Counted for Map {
    fn references(&self) -> &Count {
        &self.references
    }

    /// Frees `map`, as `vm_map_deallocate` does at zero: its whole range
    /// deleted, its physical map destroyed, then it goes back to its cache.
    ///
    /// # Panics
    ///
    /// As `Map::delete` and `MachinePhysMap::destroy`.
    unsafe fn free(map: NonNull<Self>) {
        // The box's own provenance, which `Task::create` exposed.
        let map = NonNull::<Self>::with_exposed_provenance(map.addr());
        // SAFETY: nothing else uses it, and it stays allocated until its
        // box drops below.
        let this = unsafe { map.as_ref() };
        // vm_map_delete of its bounds, under its lock, which nothing else
        // takes now (GNU Mach: unlocked). No entry is split: none reserved.
        let mut reservation = EntryReservation {
            entries: RBTree::new(EntryAdapter::NEW),
            count: 0,
        };
        let mut contents = this.lock();
        this.delete(&mut contents, this.bounds.clone(), &mut reservation);
        drop(contents);
        // SAFETY: the map's reference, given up here; its entries are gone,
        // and its task's threads were switched from for the last time.
        unsafe { PhysMap::destroy(NonNull::from(this.phys_map)) };
        // SAFETY: a `MAP_CACHE` box `Task::create` gave up, with that box's
        // provenance; nothing uses it after.
        drop(unsafe { CacheBox::from_raw(&MAP_CACHE, map) });
    }
}

/// The cache maps come from.
static MAP_CACHE: Cache<Map> = Cache::new("vm_map");

/// Sets up address maps, on the boot CPU.
pub fn init() {
    MAP_CACHE.register();
    ENTRY_CACHE.register();
    COPY_CACHE.register();
}
