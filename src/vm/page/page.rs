// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr;
use core::sync::atomic::{AtomicPtr, AtomicU8, AtomicU16, AtomicU32, AtomicUsize, Ordering};

use intrusive_collections::{
    KeyAdapter, LinkedListAtomicLink, RBTreeAtomicLink, intrusive_adapter,
};

use super::pool::pages;
use super::queue::{QUEUES, color};
use crate::arch::{PAGE_SIZE, PhysAddr};
use crate::slab::{Cache, Slab};
use crate::sync::Spinlock;
use crate::vm::Protection;
use crate::vm::object::object::Object;

/// A physical page.
pub struct Page {
    /// Its place in the page queue it is on, or in its cache's list while it
    /// is a slab.
    queue_link: LinkedListAtomicLink,
    /// Its place in its object's tree, while it belongs to one.
    object_link: RBTreeAtomicLink,
    /// The object it belongs to; null while it belongs to none. Changed
    /// only behind the object's token.
    pub(super) object: AtomicPtr<Object>,
    /// Where it is in its object, in bytes; changed as `object` is.
    pub(super) offset: AtomicUsize,
    /// Its busy word: `HARD_BUSY` while the thread that set it fills or
    /// maps it, and others keep off it.
    busy: AtomicU32,
    /// How many times it is wired, which keeps it off the active and
    /// inactive queues; changed only behind its object's token.
    pub(super) wire_count: AtomicU16,
    /// Its `Flags`; changed only behind its object's token.
    pub(super) flags: AtomicU8,
    /// The accesses its pager holds off, as `vm_prot_t` bits; behind its
    /// object's token.
    lock: AtomicU8,
    /// The accesses already asked of its pager to be unlocked; behind its
    /// object's token.
    unlock_request: AtomicU8,
    /// Its lock while it is a user page table, over its entries.
    pub table_lock: Spinlock<()>,
    /// Its state while it is a slab.
    pub slab: Slab,
}

impl Page {
    /// One on no queue, in no object.
    pub(super) const fn new() -> Self {
        Self {
            queue_link: LinkedListAtomicLink::new(),
            object_link: RBTreeAtomicLink::new(),
            object: AtomicPtr::new(ptr::null_mut()),
            offset: AtomicUsize::new(0),
            busy: AtomicU32::new(0),
            wire_count: AtomicU16::new(0),
            flags: AtomicU8::new(0),
            lock: AtomicU8::new(0),
            unlock_request: AtomicU8::new(0),
            table_lock: Spinlock::new(()),
            slab: Slab::new(),
        }
    }

    /// Records that it belongs to `object` at `offset`, behind the object's
    /// token.
    pub(in crate::vm) fn set_object(&self, object: &Object, offset: usize) {
        self.offset.store(offset, Ordering::Relaxed);
        self.object
            .store(ptr::from_ref(object).cast_mut(), Ordering::Relaxed);
    }

    /// Where it is in its object, in bytes.
    pub(in crate::vm) fn offset(&self) -> usize {
        self.offset.load(Ordering::Relaxed)
    }

    /// Makes it busy, before it goes into an object.
    pub(in crate::vm) fn set_busy(&self) {
        self.busy.fetch_or(HARD_BUSY, Ordering::Relaxed);
    }

    /// Whether it is busy, behind its object's token.
    pub(in crate::vm) fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Relaxed) & HARD_BUSY != 0
    }

    /// Makes it no longer busy and, once anything waits, wakes the waiters,
    /// behind its object's token.
    pub(in crate::vm) fn clear_busy(&self) {
        // Nothing waits on a busy page yet, so none is woken.
        self.busy.fetch_and(!HARD_BUSY, Ordering::Relaxed);
    }

    /// Whether it holds no data, its pager's not in yet, behind its object's
    /// token.
    pub(in crate::vm) fn is_absent(&self) -> bool {
        self.states().contains(Flags::ABSENT)
    }

    /// Notes that its pager's data is not in, behind its object's token.
    pub(in crate::vm) fn set_absent(&self) {
        self.set(Flags::ABSENT);
    }

    /// Notes that it holds data, behind its object's token.
    pub(in crate::vm) fn clear_absent(&self) {
        self.unset(Flags::ABSENT);
    }

    /// Whether its pager said its data is in error, behind its object's
    /// token.
    pub(in crate::vm) fn has_error(&self) -> bool {
        self.states().contains(Flags::ERROR)
    }

    /// Notes that its pager said its data is in error, behind its object's
    /// token.
    pub(in crate::vm) fn set_error(&self) {
        self.set(Flags::ERROR);
    }

    /// Whether it may have been written since it came from its pager, behind
    /// its object's token.
    pub(in crate::vm) fn is_dirty(&self) -> bool {
        self.states().contains(Flags::DIRTY)
    }

    /// Notes that it may have been written, behind its object's token.
    pub(in crate::vm) fn set_dirty(&self) {
        self.set(Flags::DIRTY);
    }

    /// Notes that it was cleaned, behind its object's token.
    pub(in crate::vm) fn clear_dirty(&self) {
        self.unset(Flags::DIRTY);
    }

    /// Whether its pager wants it back even when clean, behind its object's
    /// token.
    pub(in crate::vm) fn is_precious(&self) -> bool {
        self.states().contains(Flags::PRECIOUS)
    }

    /// Notes that its pager wants it back even when clean, behind its
    /// object's token.
    pub(in crate::vm) fn set_precious(&self) {
        self.set(Flags::PRECIOUS);
    }

    /// The accesses its pager holds off, behind its object's token.
    pub(in crate::vm) fn page_lock(&self) -> Protection {
        Protection::from_bits_retain(u32::from(self.lock.load(Ordering::Relaxed)))
    }

    /// Holds off `protection`'s accesses, behind its object's token.
    pub(in crate::vm) fn set_page_lock(&self, protection: Protection) {
        self.lock
            .store(u8::try_from(protection.bits()).unwrap(), Ordering::Relaxed);
    }

    /// The accesses already asked of its pager to be unlocked, behind its
    /// object's token.
    pub(in crate::vm) fn unlock_request(&self) -> Protection {
        Protection::from_bits_retain(u32::from(self.unlock_request.load(Ordering::Relaxed)))
    }

    /// Notes the accesses asked of its pager to be unlocked, behind its
    /// object's token.
    pub(in crate::vm) fn set_unlock_request(&self, protection: Protection) {
        self.unlock_request
            .store(u8::try_from(protection.bits()).unwrap(), Ordering::Relaxed);
    }

    /// Its `Flags`, behind its object's token.
    fn states(&self) -> Flags {
        Flags::from_bits_retain(self.flags.load(Ordering::Relaxed))
    }

    /// Sets `flag`, behind its object's token.
    fn set(&self, flag: Flags) {
        self.flags.fetch_or(flag.bits(), Ordering::Relaxed);
    }

    /// Clears `flag`, behind its object's token.
    fn unset(&self, flag: Flags) {
        self.flags.fetch_and(!flag.bits(), Ordering::Relaxed);
    }

    /// Whether it is on the active or the inactive queue, behind its
    /// object's token.
    pub(in crate::vm) fn is_on_paging_queue(&self) -> bool {
        Flags::from_bits_retain(self.flags.load(Ordering::Relaxed))
            .intersects(Flags::ACTIVE | Flags::INACTIVE)
    }

    /// Notes that it was used, behind its object's token.
    pub(in crate::vm) fn set_referenced(&self) {
        self.flags
            .fetch_or(Flags::REFERENCED.bits(), Ordering::Relaxed);
    }

    /// Puts it, referenced, at the end of its colour's active queue, behind
    /// its object's token.
    ///
    /// # Panics
    ///
    /// If it is on a paging queue or wired: not yet.
    pub(in crate::vm) fn activate(&'static self) {
        let flags = Flags::from_bits_retain(self.flags.load(Ordering::Relaxed));
        assert!(
            !flags.intersects(Flags::ACTIVE | Flags::INACTIVE),
            "vm_page_activate: a page on a paging queue, not yet"
        );
        assert!(
            self.wire_count.load(Ordering::Relaxed) == 0,
            "vm_page_activate: a wired page, not yet"
        );
        let color = color(self.address().0 / PAGE_SIZE);
        QUEUES.active.push(color, self);
        self.flags.store(
            (flags | Flags::ACTIVE | Flags::REFERENCED).bits(),
            Ordering::Relaxed,
        );
    }

    /// Takes it off the active queue, unreferenced, and puts it at the end of
    /// its colour's inactive queue unless it is wired or there already,
    /// behind its object's token.
    ///
    /// # Panics
    ///
    /// If it is inactive and referenced: not yet.
    pub(in crate::vm) fn deactivate(&'static self) {
        let mut flags = Flags::from_bits_retain(self.flags.load(Ordering::Relaxed));
        assert!(
            !flags.contains(Flags::INACTIVE | Flags::REFERENCED),
            "vm_page_deactivate: a referenced inactive page, not yet"
        );
        let color = color(self.address().0 / PAGE_SIZE);
        if flags.contains(Flags::ACTIVE) {
            // pmap_clear_reference has no counterpart until pageout reads
            // accessed bits.
            flags.remove(Flags::ACTIVE | Flags::REFERENCED);
            // SAFETY: an active page is on its colour's active queue.
            unsafe { QUEUES.active.remove(color, self) };
        }
        if self.wire_count.load(Ordering::Relaxed) == 0 && !flags.contains(Flags::INACTIVE) {
            QUEUES.inactive.push(color, self);
            flags.insert(Flags::INACTIVE);
        }
        self.flags.store(flags.bits(), Ordering::Relaxed);
    }

    /// Wires it once more, behind its object's token; wired for the first
    /// time, it leaves its paging queue.
    pub(in crate::vm) fn wire(&'static self) {
        let count = self.wire_count.load(Ordering::Relaxed);
        if count == 0 {
            self.remove_from_queue();
            QUEUES.wired_pages.fetch_add(1, Ordering::Relaxed);
        }
        self.wire_count.store(count + 1, Ordering::Relaxed);
    }

    /// Unwires it once, behind its object's token; unwired for good, it goes
    /// referenced to the end of its colour's active queue.
    ///
    /// # Panics
    ///
    /// If it is not wired.
    pub(in crate::vm) fn unwire(&'static self) {
        let count = self.wire_count.load(Ordering::Relaxed);
        assert!(count != 0, "vm_page_unwire: a page not wired");
        self.wire_count.store(count - 1, Ordering::Relaxed);
        // No page is fictitious or private yet.
        if count > 1 {
            return;
        }
        let color = color(self.address().0 / PAGE_SIZE);
        QUEUES.active.push(color, self);
        let flags = Flags::from_bits_retain(self.flags.load(Ordering::Relaxed));
        self.flags.store(
            (flags | Flags::ACTIVE | Flags::REFERENCED).bits(),
            Ordering::Relaxed,
        );
        QUEUES.wired_pages.fetch_sub(1, Ordering::Relaxed);
    }

    /// Takes it off its paging queue, if it is on one, behind its object's
    /// token.
    pub(in crate::vm) fn remove_from_queue(&'static self) {
        let flags = Flags::from_bits_retain(self.flags.load(Ordering::Relaxed));
        let color = color(self.address().0 / PAGE_SIZE);
        if flags.contains(Flags::ACTIVE) {
            // SAFETY: an active page is on its colour's active queue.
            unsafe { QUEUES.active.remove(color, self) };
        } else if flags.contains(Flags::INACTIVE) {
            // SAFETY: an inactive page is on its colour's inactive queue.
            unsafe { QUEUES.inactive.remove(color, self) };
        }
        self.flags.store(
            (flags - Flags::ACTIVE - Flags::INACTIVE).bits(),
            Ordering::Relaxed,
        );
    }

    /// Fills it with zeros.
    ///
    /// # Safety
    ///
    /// Nothing else may use its memory meanwhile.
    pub(in crate::vm) unsafe fn zero_fill(&'static self) {
        // SAFETY: the caller has the page to itself, and the direct map
        // reaches all of it.
        unsafe {
            self.address()
                .direct_map_ptr::<u8>()
                .write_bytes(0, PAGE_SIZE);
        }
    }

    /// Fills it with a copy of `source`, another page.
    ///
    /// # Safety
    ///
    /// Nothing else may use its memory meanwhile, nor write `source`'s.
    pub(in crate::vm) unsafe fn copy_from(&'static self, source: &'static Self) {
        // SAFETY: the caller has the page to itself and `source` unchanged,
        // two frames the direct map reaches all of.
        unsafe {
            self.address()
                .direct_map_ptr::<u8>()
                .copy_from_nonoverlapping(source.address().direct_map_ptr(), PAGE_SIZE);
        }
    }

    /// Where the page is.
    ///
    /// # Panics
    ///
    /// If the page array is not set up.
    pub fn address(&'static self) -> PhysAddr {
        let array = pages();
        let index = (ptr::from_ref(self).addr() - array.pages.as_ptr().addr()) / size_of::<Self>();
        array.start + index * PAGE_SIZE
    }
}

/// The busy word's bit for a page one thread has to itself.
const HARD_BUSY: u32 = 1 << 31;

bitflags::bitflags! {
    /// A page's paging queue, its data's state, and whether it was used.
    #[derive(Clone, Copy)]
    pub(super) struct Flags: u8 {
        /// On its colour's active queue.
        const ACTIVE = 1;
        /// On its colour's inactive queue.
        const INACTIVE = 1 << 1;
        /// Used since it was last deactivated.
        const REFERENCED = 1 << 2;
        /// Its pager's data is not in.
        const ABSENT = 1 << 3;
        /// Its pager said its data is in error.
        const ERROR = 1 << 4;
        /// It may have been written since it came from its pager.
        const DIRTY = 1 << 5;
        /// Its pager wants it back even when clean.
        const PRECIOUS = 1 << 6;
    }
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub PageTreeAdapter = &'static Page: Page { object_link => RBTreeAtomicLink }
);

impl<'a> KeyAdapter<'a> for PageTreeAdapter {
    type Key = usize;

    fn get_key(&self, page: &'a Page) -> usize {
        page.offset.load(Ordering::Relaxed)
    }
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    pub PageQueueAdapter = &'static Page: Page { queue_link => LinkedListAtomicLink }
);

/// The cache fictitious pages come from: pages outside the page array, such
/// as placeholders.
pub(super) static CACHE: Cache<Page> = Cache::new("vm_page");
