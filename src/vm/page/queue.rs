// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::sync::atomic::{AtomicUsize, Ordering};

use intrusive_collections::LinkedList;

use super::page::{Page, PageQueueAdapter};
use crate::sync::{CachePadded, Spinlock};

/// How many colours each page queue has; a page's colour is the exclusive
/// or of its frame number's three lowest digits in base `COLORS`.
pub(super) const COLORS: usize = 1024;

/// The colour of frame number `frame`.
pub(super) const fn color(frame: usize) -> usize {
    (frame ^ (frame / COLORS) ^ (frame / (COLORS * COLORS))) % COLORS
}

/// The pages of one colour on one queue.
pub(super) struct PageQueue {
    /// How many pages its list holds; changed only under the list's lock,
    /// and read without it to pass an empty queue by.
    pub(super) pages: AtomicUsize,
    /// The pages.
    list: Spinlock<LinkedList<PageQueueAdapter>>,
    /// Where a search from it last found a page after passing 8 or more
    /// empty queues, as that colour plus one; 0 for none, or once a page is
    /// found on it first. A hint, so Relaxed: it only moves where a search
    /// starts.
    pub(super) last: AtomicUsize,
}

impl PageQueue {
    /// An empty one.
    const fn new() -> Self {
        Self {
            pages: AtomicUsize::new(0),
            list: Spinlock::new(LinkedList::new(PageQueueAdapter::NEW)),
            last: AtomicUsize::new(0),
        }
    }

    /// Puts `page` at its end, counting it in its pool's `total` too.
    fn push(&self, page: &'static Page, total: &AtomicUsize) {
        let mut list = self.list.lock();
        list.push_back(page);
        // The count changes only under the lock.
        self.pages
            .store(self.pages.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
        total.fetch_add(1, Ordering::Relaxed);
        drop(list);
    }

    /// Takes `page` off it, dropping it from its pool's `total` too.
    ///
    /// # Safety
    ///
    /// `page` must be on it.
    unsafe fn remove(&self, page: &'static Page, total: &AtomicUsize) {
        let mut list = self.list.lock();
        // SAFETY: the caller's.
        unsafe { list.cursor_mut_from_ptr(page).remove() };
        // The count changes only under the lock.
        self.pages
            .store(self.pages.load(Ordering::Relaxed) - 1, Ordering::Relaxed);
        total.fetch_sub(1, Ordering::Relaxed);
        drop(list);
    }

    /// Takes its first page, dropping it from its pool's `total` too; an
    /// empty queue is passed by without its lock.
    fn pop(&self, total: &AtomicUsize) -> Option<&'static Page> {
        if self.pages.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let mut list = self.list.lock();
        let page = list.pop_front()?;
        // The count changes only under the lock.
        self.pages
            .store(self.pages.load(Ordering::Relaxed) - 1, Ordering::Relaxed);
        total.fetch_sub(1, Ordering::Relaxed);
        drop(list);
        Some(page)
    }
}

/// A page queue per colour, each on its own cache lines, and how many
/// pages they hold.
pub(super) struct ColorQueues {
    /// How many pages its queues hold; changed under the lock of the queue
    /// changed.
    pub(super) pages: AtomicUsize,
    /// Its queues, by colour.
    pub(super) queues: [CachePadded<PageQueue>; COLORS],
}

impl ColorQueues {
    /// Empty ones.
    #[expect(
        clippy::large_stack_arrays,
        reason = "only QUEUES' initializer calls it, at compile time"
    )]
    const fn new() -> Self {
        Self {
            pages: AtomicUsize::new(0),
            queues: [const { CachePadded::new(PageQueue::new()) }; COLORS],
        }
    }

    /// Puts `page` at the end of `color`'s queue.
    pub(super) fn push(&self, color: usize, page: &'static Page) {
        self.queues[color].push(page, &self.pages);
    }

    /// Takes `page` off `color`'s queue.
    ///
    /// # Safety
    ///
    /// `page` must be on it.
    pub(super) unsafe fn remove(&self, color: usize, page: &'static Page) {
        // SAFETY: the caller's.
        unsafe { self.queues[color].remove(page, &self.pages) };
    }

    /// Takes the first page of `color`'s queue.
    pub(super) fn pop(&self, color: usize) -> Option<&'static Page> {
        self.queues[color].pop(&self.pages)
    }
}

/// The page queues, by use, and how many pages are wired.
pub(super) struct PageQueues {
    /// How many pages are wired.
    pub(super) wired_pages: AtomicUsize,
    /// Free pages.
    pub(super) free: ColorQueues,
    /// Pages in use, not referenced lately.
    pub(super) inactive: ColorQueues,
    /// Pages in use, referenced lately.
    pub(super) active: ColorQueues,
}

pub(super) static QUEUES: PageQueues = PageQueues {
    wired_pages: AtomicUsize::new(0),
    free: ColorQueues::new(),
    inactive: ColorQueues::new(),
    active: ColorQueues::new(),
};
