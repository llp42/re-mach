// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Address maps.

use core::ops::Range;
use core::sync::atomic::{AtomicUsize, Ordering};

use intrusive_collections::{
    Bound, KeyAdapter, RBTree, RBTreeAtomicLink, UnsafeRef, intrusive_adapter,
};

use super::pmap::MachinePmap as _;
use super::{Inheritance, Protection};
use crate::arch::Pmap;
use crate::kernel::Error;
use crate::kernel::slab::{Cache, CacheBox};
use crate::sync::{OnceLock, SleepLock};

/// An address map: ranges of virtual memory and what they map; so far only
/// what a new map sets.
#[expect(dead_code, reason = "pmap_enter and vm_map_reference read them")]
pub struct Map {
    /// The addresses it may map.
    bounds: Range<usize>,
    /// The physical map its mappings go into.
    pmap: &'static Pmap,
    /// How many references it has.
    references: u32,
    /// What changes as it maps and unmaps, behind its lock.
    contents: SleepLock<Contents>,
}

/// What a map's lock guards.
struct Contents {
    /// Its entries, by address.
    entries: RBTree<EntryAdapter>,
    /// The entries free space follows, by its size, then by address.
    gaps: RBTree<GapAdapter>,
    /// The free space before the first entry; all of the map while it is
    /// empty.
    leading_gap: usize,
    /// How much it maps, in bytes.
    size: usize,
}

impl Map {
    /// An empty map of `bounds` over `pmap`, with one reference.
    pub const fn new(pmap: &'static Pmap, bounds: Range<usize>) -> Self {
        let leading_gap = bounds.end - bounds.start;
        Self {
            bounds,
            pmap,
            references: 1,
            contents: SleepLock::new(Contents {
                entries: RBTree::new(EntryAdapter::NEW),
                gaps: RBTree::new(GapAdapter::NEW),
                leading_gap,
                size: 0,
            }),
        }
    }

    /// Maps `range` with nothing behind it yet, allowing `protection` now
    /// and at most `max_protection`, and giving child tasks `inheritance`.
    ///
    /// # Errors
    ///
    /// `InvalidAddress` if `range` is empty or not inside the map, `NoSpace`
    /// if it overlaps an entry, and `ResourceShortage` if no entry can be
    /// allocated.
    pub fn enter(
        &self,
        range: Range<usize>,
        protection: Protection,
        max_protection: Protection,
        inheritance: Inheritance,
    ) -> Result<(), Error> {
        let mut contents = self.contents.write();
        if range.start < self.bounds.start || range.end > self.bounds.end || range.is_empty() {
            return Err(Error::InvalidAddress);
        }
        let below = contents.entries.upper_bound(Bound::Included(&range.start));
        let above = contents.entries.lower_bound(Bound::Excluded(&range.start));
        if below
            .get()
            .is_some_and(|entry| entry.range.end > range.start)
            || above
                .get()
                .is_some_and(|entry| entry.range.start < range.end)
        {
            return Err(Error::NoSpace);
        }
        let previous = below.clone_pointer();
        let next = above
            .get()
            .map_or(self.bounds.end, |entry| entry.range.start);
        let gap = next - range.end;
        let entry = ENTRY_CACHE.allocate(Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            range,
            gap: AtomicUsize::new(gap),
            protection,
            max_protection,
            inheritance,
            backing: None,
        })?;
        contents.link(self.bounds.start, previous, entry);
        drop(contents);
        Ok(())
    }

    /// Maps a new submap of `size` bytes wherever it fits, over this map's
    /// physical map, and sets `submap` to it.
    ///
    /// # Errors
    ///
    /// `NoSpace` if no free space fits it, and `ResourceShortage` if no
    /// entry can be allocated, which leaves `submap` set.
    ///
    /// # Panics
    ///
    /// If `submap` is set already.
    pub fn enter_submap(&self, size: usize, submap: &'static OnceLock<Self>) -> Result<(), Error> {
        let mut contents = self.contents.write();
        let (previous, start) = contents.find_space(self.bounds.start, size)?;
        let next = previous
            .as_ref()
            .map_or(self.bounds.start + contents.leading_gap, |entry| {
                entry.range.end + entry.gap.load(Ordering::Relaxed)
            });
        let range = start..start + size;
        self.pmap.reference();
        assert!(
            submap.set(Self::new(self.pmap, range.clone())).is_ok(),
            "vm_map: submap set twice"
        );
        let entry = ENTRY_CACHE.allocate(Entry {
            tree_link: RBTreeAtomicLink::new(),
            gap_link: RBTreeAtomicLink::new(),
            gap: AtomicUsize::new(next - range.end),
            range,
            protection: Protection::READ | Protection::WRITE,
            max_protection: Protection::all(),
            inheritance: Inheritance::Copy,
            backing: submap.get().map(Backing::Submap),
        })?;
        contents.link(self.bounds.start, previous, entry);
        drop(contents);
        Ok(())
    }
}

impl Contents {
    /// Where `size` bytes fit, and the entry they would follow: after the
    /// smallest gap that holds them, the lowest among equals, or else at the
    /// map's `start`, if the leading gap holds them.
    fn find_space(
        &self,
        start: usize,
        size: usize,
    ) -> Result<(Option<UnsafeRef<Entry>>, usize), Error> {
        let fit = self.gaps.lower_bound(Bound::Included(&(size, 0)));
        match fit.get() {
            Some(entry) => Ok((fit.clone_pointer(), entry.range.end)),
            None if self.leading_gap >= size => Ok((None, start)),
            None => Err(Error::NoSpace),
        }
    }

    /// Links `entry` after `previous`, or first, keeping the gaps and size
    /// up to date; it fits between `previous` and the entry after it, or
    /// the map's `start` and end.
    fn link(&mut self, start: usize, previous: Option<UnsafeRef<Entry>>, entry: CacheBox<Entry>) {
        let (range, gap) = (entry.range.clone(), entry.gap.load(Ordering::Relaxed));
        // The entry now ends the gap after the one before it, or the leading
        // gap.
        match previous {
            Some(previous) => {
                if previous.gap_link.is_linked() {
                    let key = (previous.gap.load(Ordering::Relaxed), previous.range.start);
                    self.gaps.find_mut(&key).remove();
                }
                let previous_gap = range.start - previous.range.end;
                previous.gap.store(previous_gap, Ordering::Relaxed);
                if previous_gap != 0 {
                    self.gaps.insert(previous);
                }
            }
            None => self.leading_gap = range.start - start,
        }
        let entry = CacheBox::into_raw(entry).as_ptr();
        // SAFETY: the trees take the entry for good, once each.
        self.entries.insert(unsafe { UnsafeRef::from_raw(entry) });
        if gap != 0 {
            // SAFETY: as above.
            self.gaps.insert(unsafe { UnsafeRef::from_raw(entry) });
        }
        self.size += range.end - range.start;
    }
}

/// A range of a map and what it maps; so far only what an entry with no
/// object or a submap sets.
pub struct Entry {
    /// Its place in its map's tree.
    tree_link: RBTreeAtomicLink,
    /// Its place in its map's gap tree, while free space follows it.
    gap_link: RBTreeAtomicLink,
    /// The addresses it covers.
    range: Range<usize>,
    /// The free space after it, up to the next entry or the map's end;
    /// changed only under the map's write lock.
    gap: AtomicUsize,
    /// What it allows now.
    #[expect(dead_code, reason = "vm_fault and vm_map_protect read it")]
    protection: Protection,
    /// The most it may ever allow.
    #[expect(dead_code, reason = "vm_map_protect reads it")]
    max_protection: Protection,
    /// What a child task gets of it.
    #[expect(dead_code, reason = "vm_map_fork reads it")]
    inheritance: Inheritance,
    /// What backs it; nothing while it is only reserved.
    #[expect(dead_code, reason = "vm_map_lookup and vm_fault read it")]
    backing: Option<Backing>,
}

/// What backs an entry; only the kinds used here.
pub enum Backing {
    /// A map of its own over the same physical map, such as the kernel's
    /// IPC buffers.
    Submap(#[expect(dead_code, reason = "vm_map_lookup descends into it")] &'static Map),
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    EntryAdapter = UnsafeRef<Entry>: Entry { tree_link => RBTreeAtomicLink }
);

impl<'a> KeyAdapter<'a> for EntryAdapter {
    type Key = usize;

    fn get_key(&self, entry: &'a Entry) -> usize {
        entry.range.start
    }
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    GapAdapter = UnsafeRef<Entry>: Entry { gap_link => RBTreeAtomicLink }
);

impl<'a> KeyAdapter<'a> for GapAdapter {
    type Key = (usize, usize);

    fn get_key(&self, entry: &'a Entry) -> (usize, usize) {
        (entry.gap.load(Ordering::Relaxed), entry.range.start)
    }
}

/// Memory copied out of a map while in transit, such as out-of-line data; so
/// far only its kind and size.
#[expect(dead_code, reason = "vm_map_copyin and vm_map_copyout use them")]
pub struct MapCopy {
    /// How it holds the memory.
    kind: CopyKind,
    /// Its size, in bytes.
    size: usize,
}

/// How a copy holds its memory.
#[expect(dead_code, reason = "vm_map_copyin and vm_map_copyout use them")]
enum CopyKind {
    /// As map entries, like a map.
    Entries,
    /// As one object, mostly on the pageout path.
    Object,
    /// As a list of pages.
    Pages,
}

/// The cache maps come from.
static MAP_CACHE: Cache<Map> = Cache::new("vm_map");

/// The cache entries come from.
static ENTRY_CACHE: Cache<Entry> = Cache::new("vm_map_entry");

/// The cache copies come from.
static COPY_CACHE: Cache<MapCopy> = Cache::new("vm_map_copy");

/// Sets up address maps, on the boot CPU.
pub fn init() {
    MAP_CACHE.register();
    ENTRY_CACHE.register();
    COPY_CACHE.register();
}
