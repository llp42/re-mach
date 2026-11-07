// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Memory objects' kernel side.

use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use intrusive_collections::{
    LinkedList, LinkedListAtomicLink, RBTree, UnsafeRef, intrusive_adapter,
};

use super::page::{Page, PageTreeAdapter};
use crate::arch::{MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE};
use crate::ipc::port::{self, KernelObject, Port};
use crate::slab::{Cache, CacheBox};
use crate::sync::{Token, TokenGuard};

/// The kernel's side of a memory object; so far only its size, references
/// and name port, its place in the list of persisting objects and its state.
pub struct Object {
    /// Its size, in bytes.
    #[expect(dead_code, reason = "vm_object_coalesce and the pager calls read it")]
    size: usize,
    /// How many references it has.
    references: AtomicU32,
    /// Its name port, which names it to `vm_region`; null until it has one.
    pager_name: AtomicPtr<Port>,
    /// Its place in the list of persisting objects.
    persisting_link: LinkedListAtomicLink,
    /// Its state, behind its token.
    state: Token<ObjectState>,
}

/// What an object's token guards; so far only its pages, where the last
/// one went in, its paging under way and its first values.
#[expect(
    dead_code,
    reason = "vm_object_deallocate's last reference and the pager calls read them"
)]
#[expect(clippy::struct_excessive_bools, reason = "GNU Mach's object flags")]
pub struct ObjectState {
    /// Its resident pages.
    pages: Pages,
    /// The offset its last page went in at.
    last_alloc: usize,
    /// The object its pages are copied into before they change; none yet.
    pub(super) copy: Option<&'static Object>,
    /// The object behind it, whose pages show where it has none; none yet.
    pub(super) shadow: Option<&'static Object>,
    /// How many paging operations on it are under way.
    paging_in_progress: u32,
    /// Whether a pager was ever made for it; none is yet.
    pub(super) pager_created: bool,
    /// Whether the kernel created it, so the default memory manager manages it.
    internal: bool,
    /// Whether changes stay in memory rather than going back to its manager.
    temporary: bool,
    /// Whether it is not yet terminated.
    alive: bool,
}

/// An object's resident pages.
struct Pages {
    /// The pages, by offset.
    tree: RBTree<PageTreeAdapter>,
    /// How many there are.
    count: usize,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    PersistingAdapter = UnsafeRef<Object>: Object { persisting_link => LinkedListAtomicLink }
);

impl Object {
    /// A new object of `size` bytes, with one reference.
    const fn new(size: usize) -> Self {
        Self {
            size,
            references: AtomicU32::new(1),
            pager_name: AtomicPtr::new(ptr::null_mut()),
            persisting_link: LinkedListAtomicLink::new(),
            state: Token::new(ObjectState {
                pages: Pages {
                    tree: RBTree::new(PageTreeAdapter::NEW),
                    count: 0,
                },
                last_alloc: 0,
                copy: None,
                shadow: None,
                paging_in_progress: 0,
                pager_created: false,
                internal: true,
                temporary: true,
                alive: true,
            }),
        }
    }

    /// Takes a reference on it.
    pub fn reference(&self) {
        self.references.fetch_add(1, Ordering::Relaxed);
    }

    /// Drops a reference on it, which is not its last.
    ///
    /// # Panics
    ///
    /// On its last reference: not yet.
    pub fn deallocate(&self) {
        // Only the last reference's drop needs the token.
        assert!(
            self.references
                .try_update(Ordering::Release, Ordering::Relaxed, |references| {
                    (references > 1).then(|| references - 1)
                })
                .is_ok(),
            "vm_object_deallocate: the last reference, not yet"
        );
    }

    /// Takes its token.
    pub fn lock(&self) -> TokenGuard<'_, ObjectState> {
        self.state.lock()
    }
}

impl ObjectState {
    /// Notes a paging operation under way.
    pub const fn begin_paging(&mut self) {
        self.paging_in_progress += 1;
    }

    /// Notes the end of a paging operation under way.
    ///
    /// # Panics
    ///
    /// If none is under way.
    pub(super) const fn end_paging(&mut self) {
        assert!(self.paging_in_progress != 0, "vm_object_paging_end");
        self.paging_in_progress -= 1;
        // Nothing waits for paging to end yet, so none is woken.
    }

    /// Puts `page`, which belongs to no object, in `object`, whose state
    /// this is, at `offset`; the page put in last, if just before it and
    /// not busy, is deactivated.
    ///
    /// # Panics
    ///
    /// If `page` belongs to an object, or as `Page::deactivate`.
    pub(super) fn insert(&mut self, object: &Object, page: &'static Page, offset: usize) {
        page.set_object(object, offset);
        self.pages.tree.insert(page);
        self.pages.count += 1;
        // Sequential access: the page before is no longer interesting.
        if offset == self.last_alloc + PAGE_SIZE
            && let Some(last) = self.lookup(self.last_alloc)
            && !last.is_busy()
        {
            last.deactivate();
        }
        self.last_alloc = offset;
    }

    /// The page at `offset`; `None` if none is resident.
    pub(super) fn lookup(&self, offset: usize) -> Option<&'static Page> {
        self.pages.tree.find(&offset).clone_pointer()
    }
}

/// The object all wired kernel memory belongs to, at the offset of its
/// address in the kernel map.
pub static KERNEL_OBJECT: Object = Object::new(MAX_KERNEL_ADDRESS - MIN_KERNEL_ADDRESS + 1);

/// The unreferenced objects kept in case they are used again, until memory
/// runs low.
#[expect(dead_code, reason = "vm_object_deallocate and vm_object_lookup use it")]
static PERSISTING: LinkedList<PersistingAdapter> = LinkedList::new(PersistingAdapter::NEW);

/// The cache objects come from.
static CACHE: Cache<Object> = Cache::new("vm_object");

/// Sets up memory objects, on the boot CPU.
pub fn bootstrap() {
    CACHE.register();
}

/// Gives the kernel object its name port, on the boot CPU.
///
/// # Panics
///
/// If no port can be allocated.
pub fn init() {
    let port = port::allocate_kernel()
        .unwrap_or_else(|error| panic!("vm_object_init: ipc_port_alloc_kernel failed: {error:?}"));
    port.set_kernel_object(KernelObject::PagerName(NonNull::from(&KERNEL_OBJECT)));
    KERNEL_OBJECT
        .pager_name
        .store(CacheBox::into_raw(port).as_ptr(), Ordering::Release);
}

/// A new internal, temporary object of `size` bytes, with one reference
/// and a name port.
///
/// # Panics
///
/// If the object or its port can't be allocated.
pub fn allocate(size: usize) -> &'static Object {
    let object = CACHE
        .allocate(Object::new(size))
        .unwrap_or_else(|error| panic!("vm_object_allocate: {error:?}"));
    // SAFETY: an object lives until vm_object_deallocate frees it, which
    // comes later.
    let object = unsafe { CacheBox::into_raw(object).as_ref() };
    let port =
        port::allocate_kernel().unwrap_or_else(|error| panic!("vm_object_allocate: {error:?}"));
    port.set_kernel_object(KernelObject::PagerName(NonNull::from(object)));
    object
        .pager_name
        .store(CacheBox::into_raw(port).as_ptr(), Ordering::Release);
    object
}
