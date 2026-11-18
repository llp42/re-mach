// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Memory objects' kernel side.

use core::mem::ManuallyDrop;
use core::ops::Add;
use core::ptr::{self, NonNull};
use core::sync::atomic::{AtomicPtr, AtomicU32, Ordering};

use intrusive_collections::{
    LinkedList, LinkedListAtomicLink, RBTree, UnsafeRef, intrusive_adapter,
};

use super::STATISTICS;
use super::page::{Page, PageTreeAdapter};
use crate::arch::{MAX_KERNEL_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE};
use crate::device::pager::{self, Pager};
use crate::ipc::port::{KernelObject, Port};
use crate::reference::Reference;
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

/// A place in an object, in bytes from its start.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Offset(usize);

impl From<usize> for Offset {
    fn from(offset: usize) -> Self {
        Self(offset)
    }
}

impl From<Offset> for usize {
    fn from(offset: Offset) -> Self {
        offset.0
    }
}

impl Add<usize> for Offset {
    type Output = Self;

    fn add(self, size: usize) -> Self {
        Self(self.0 + size)
    }
}

/// What an object's token guards; so far only its pages, where the last
/// one went in, its paging under way, its pager and its first values.
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
    pub(super) paging_in_progress: u32,
    /// Its `Flags`.
    pub(super) flags: Flags,
    /// What serves its pages; none until a pager is entered for it.
    pub(super) kind: Option<Kind>,
    /// A send right for its pager, the memory object; none until it has one.
    pager: Option<Reference<Port>>,
    /// Its request port, which its pager calls the kernel on; none until it
    /// has a pager.
    pager_request: Option<Reference<Port>>,
    /// How its data is copied.
    copy_strategy: CopyStrategy,
}

/// What serves an object's pages; only the kinds used here.
pub(super) enum Kind {
    /// A device's memory, through its pager's record.
    Device(UnsafeRef<Pager>),
}

/// How an object's data is copied, as Mach's
/// `memory_object_copy_strategy_t` names it; only the values used here.
pub enum CopyStrategy {
    /// `MEMORY_OBJECT_COPY_NONE`: by reading it all, with no help.
    None,
}

/// An object's resident pages.
struct Pages {
    /// The pages, by offset.
    tree: RBTree<PageTreeAdapter>,
    /// How many there are.
    count: usize,
}

bitflags::bitflags! {
    /// Whether an object had a pager, how it is managed and whether it lives.
    pub(super) struct Flags: u8 {
        /// A pager was ever made for it.
        const PAGER_CREATED = 1;
        /// The kernel created it, so the default memory manager manages it.
        const INTERNAL = 1 << 1;
        /// Changes stay in memory rather than going back to its manager.
        const TEMPORARY = 1 << 2;
        /// Not yet terminated.
        const ALIVE = 1 << 3;
        /// Its pager was set up.
        const PAGER_INITIALIZED = 1 << 4;
        /// Its pager said it is ready.
        const PAGER_READY = 1 << 5;
        /// It stays, unreferenced, among the persisting objects.
        const CAN_PERSIST = 1 << 6;
    }
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
                flags: Flags::INTERNAL.union(Flags::TEMPORARY).union(Flags::ALIVE),
                kind: None,
                pager: None,
                pager_request: None,
                copy_strategy: CopyStrategy::None,
            }),
        }
    }

    /// Takes a reference on it.
    pub fn reference(&self) {
        self.references.fetch_add(1, Ordering::Relaxed);
    }

    /// How many references it has.
    pub(super) fn references(&self) -> u32 {
        self.references.load(Ordering::Relaxed)
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

/// How many objects pagers outside the kernel's default one manage, as
/// `vm_statistics` reports them.
static EXTERNAL_COUNT: AtomicU32 = AtomicU32::new(0);

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
    let port = Port::allocate_kernel()
        .unwrap_or_else(|error| panic!("vm_object_init: ipc_port_alloc_kernel failed: {error:?}"));
    port.set_kernel_object(KernelObject::PagerName(NonNull::from(&KERNEL_OBJECT)));
    // The slot keeps its reference.
    KERNEL_OBJECT.pager_name.store(
        Reference::into_raw(Reference::from(port)).as_ptr(),
        Ordering::Release,
    );
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
        Port::allocate_kernel().unwrap_or_else(|error| panic!("vm_object_allocate: {error:?}"));
    port.set_kernel_object(KernelObject::PagerName(NonNull::from(object)));
    // The slot keeps its reference.
    object.pager_name.store(
        Reference::into_raw(Reference::from(port)).as_ptr(),
        Ordering::Release,
    );
    object
}

/// The object and offset two adjoining ranges backed by `previous` and
/// `next`, none for no object, have once joined.
///
/// # Panics
///
/// Unless neither has an object: not yet.
pub fn coalesce(
    previous: Option<&'static Object>,
    next: Option<&'static Object>,
) -> (Option<&'static Object>, Offset) {
    match (previous, next) {
        // Neither exists, so the offsets don't matter.
        (None, None) => (None, Offset::from(0)),
        (Some(previous), Some(next)) if ptr::eq(previous, next) => {
            panic!("vm_object_coalesce: the same object, not yet")
        }
        (Some(_), Some(_)) => panic!("vm_object_coalesce: two different objects, not yet"),
        _ => panic!("vm_object_coalesce: one object, not yet"),
    }
}

/// The object of the memory object `pager`, of `size` bytes, made and set
/// up with its pager, as `vm_object_enter` gives it for an object outside
/// the kernel, with a reference for the caller; none if `pager` stands for
/// something else.
///
/// # Panics
///
/// If `pager` has an object, another call made one meanwhile, or `pager`
/// is a user's: not yet. If no request port can be allocated. As
/// `allocate` and `pager::init_pager`.
pub fn enter(pager: &Reference<Port>, size: usize) -> Option<&'static Object> {
    // A device pager's record, or none for a user's pager.
    let record = match pager.kernel_object() {
        Some(KernelObject::DevicePager(record)) => Some(record),
        None => None,
        Some(KernelObject::Pager(_)) => panic!("vm_object_enter: an existing object, not yet"),
        Some(_) => {
            STATISTICS.lookups.fetch_add(1, Ordering::Relaxed);
            return None;
        }
    };
    // Made first; setting it re-checks the port.
    let object = allocate(size);
    assert!(
        matches!(
            pager.replace_kernel_object(KernelObject::Pager(NonNull::from(object))),
            Some(KernelObject::DevicePager(_)) | None
        ),
        "vm_object_enter: an object made meanwhile, not yet"
    );
    STATISTICS.lookups.fetch_add(1, Ordering::Relaxed);
    let pager = Port::copy_send(pager);
    let request = Reference::from(
        Port::allocate_kernel()
            .unwrap_or_else(|error| panic!("vm_object_enter: pager request alloc: {error:?}")),
    );
    request.set_kernel_object(KernelObject::PagingRequest(NonNull::from(object)));
    let mut state = object.lock();
    state.flags.insert(Flags::PAGER_CREATED);
    state.pager = Some(pager);
    state.pager_request = Some(request);
    // External, not temporary, and not ready until its pager says so.
    state.flags.remove(Flags::INTERNAL | Flags::TEMPORARY);
    // SAFETY: a pager's record stays allocated while its device lists it,
    // as long as its object lives.
    state.kind = record.map(|record| Kind::Device(unsafe { UnsafeRef::from_raw(record.as_ptr()) }));
    drop(state);
    EXTERNAL_COUNT.fetch_add(1, Ordering::Relaxed);
    // memory_object_init: its rights, made from the request and name ports,
    // then the kernel's own pager called directly.
    let request_send = Port::make_send(object.lock().pager_request.as_ref().unwrap());
    // SAFETY: the object's slot holds a reference to its name port, which
    // this handle borrows and never drops.
    let name = ManuallyDrop::new(unsafe {
        Reference::from_raw(NonNull::new(object.pager_name.load(Ordering::Acquire)).unwrap())
    });
    let name_send = Port::make_send(&name);
    let record = record.unwrap_or_else(|| panic!("memory_object_init: a user pager, not yet"));
    // SAFETY: as for its kind above.
    pager::init_pager(unsafe { record.as_ref() }, request_send, name_send);
    object.lock().flags.insert(Flags::PAGER_INITIALIZED);
    // Nothing waits for it to be initialized yet, so none is woken.
    Some(object)
}

/// The object whose request port `port` is, with a reference taken, as
/// `vm_object_lookup` gives it; none if it is no request port.
///
/// # Panics
///
/// If the object has no reference: not yet.
fn lookup(port: &Port) -> Option<&'static Object> {
    port.with_kernel_object(|kernel_object| match kernel_object {
        Some(&KernelObject::PagingRequest(object)) => {
            // SAFETY: an object outlives its request port's standing for it.
            let object = unsafe { object.as_ref() };
            assert!(
                object.references() != 0,
                "vm_object_lookup: vm_object_cache_remove, not yet"
            );
            object.reference();
            Some(object)
        }
        _ => None,
    })
}

/// Readies the object whose request port `request` is, as its pager's
/// `memory_object_ready` asks, with `memory_object_set_attributes_common`:
/// not to persist, copied as `copy_strategy` says.
///
/// # Panics
///
/// If `request` is no object's request port: not yet. As `lookup` and
/// `Object::deallocate`.
pub fn ready(request: &Port, copy_strategy: CopyStrategy) {
    let object = lookup(request)
        .unwrap_or_else(|| panic!("memory_object_ready: KERN_INVALID_ARGUMENT, not yet"));
    let mut state = object.lock();
    // Nothing waits for it to be ready yet, so none is woken.
    state.flags.remove(Flags::CAN_PERSIST);
    state.flags.insert(Flags::PAGER_READY);
    state.copy_strategy = copy_strategy;
    drop(state);
    object.deallocate();
}
