// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Global synchronisation: threads that wait on a word of memory, hashed
//! into buckets by where the word is.

use core::ptr;

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

use crate::arch::{self, Machine as _};
use crate::error::Error;
use crate::reference::Reference;
use crate::sched::{self, WaitResult};
use crate::smp;
use crate::sync::{CachePadded, Token};
use crate::task::Task;
use crate::thread::thread::Thread;
use crate::vm::{Protection, VirtualAddress};

bitflags::bitflags! {
    /// How a thread waits on a word or wakes its waiters, as gsync's flags
    /// number them; only those used here.
    #[derive(Clone, Copy)]
    pub struct Flags: i32 {
        /// The word may be shared between tasks.
        const SHARED = 0x01;
        /// The word after it must match too.
        const QUAD = 0x02;
        /// The wait ends after a timeout.
        const TIMED = 0x04;
        /// Every waiter is woken, not only the first.
        const BROADCAST = 0x08;
        /// The word is to be written.
        const MUTATE = 0x10;
    }
}

/// Where a word waited on is: so far a word of one task, by its map and
/// its address there. Keys order by map, then address.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    /// The map's address.
    map: usize,
    /// The word's address in the map.
    address: VirtualAddress,
}

impl Key {
    /// The bucket its waiters go in, by GNU Mach's hash of its halves.
    fn bucket(&self) -> &'static Bucket {
        let hash = [self.map, usize::from(self.address)]
            .into_iter()
            .flat_map(|word| [word & 0xffff_ffff, word >> 32])
            .fold(u32::try_from(size_of::<usize>()).unwrap(), |hash, half| {
                hash.rotate_left(5) ^ u32::try_from(half).unwrap()
            });
        &BUCKETS[usize::try_from(hash).unwrap() % BUCKET_COUNT]
    }
}

/// A thread waiting on a word.
pub struct Waiter {
    /// Its place in its bucket.
    link: LinkedListAtomicLink,
    /// Where the word is.
    key: Key,
    /// The thread.
    thread: &'static Thread,
}

intrusive_adapter!(
    #[expect(
        clippy::expl_impl_clone_on_copy,
        reason = "intrusive_adapter! writes Clone by hand"
    )]
    WaiterAdapter = UnsafeRef<Waiter>: Waiter { link => LinkedListAtomicLink }
);

/// The threads waiting on the words that hash to one bucket.
struct Bucket {
    /// The waiters, in key order, each key's in the order they came.
    waiters: Token<LinkedList<WaiterAdapter>>,
}

/// How many buckets there are.
const BUCKET_COUNT: usize = 512;

/// The buckets, each alone on its cache lines.
static BUCKETS: [CachePadded<Bucket>; BUCKET_COUNT] = [const {
    CachePadded::new(Bucket {
        waiters: Token::new(LinkedList::new(WaiterAdapter::NEW)),
    })
}; BUCKET_COUNT];

/// Makes the current thread wait on the word at `address` in `task` if it
/// holds `value` (and the word after it `value2`, with `QUAD`), until a
/// wake or, with `TIMED`, `milliseconds` later, as `gsync_wait` does.
///
/// # Errors
///
/// `InvalidArgument` if a word differs, before any wait; `TimedOut` if the
/// time ran out first.
///
/// # Panics
///
/// If no thread runs on the CPU; as `Map::probe`. For no task, a misaligned
/// address, one `probe_address` refuses, a shared word, another task or a
/// word that can't be read: not yet.
pub fn wait(
    task: Option<&Task>,
    address: VirtualAddress,
    value: u32,
    value2: u32,
    milliseconds: u32,
    flags: Flags,
) -> Result<(), Error> {
    let Some(task) = task else {
        panic!("gsync_wait: KERN_INVALID_TASK, not yet");
    };
    assert!(
        usize::from(address) % size_of::<u32>() == 0,
        "gsync_wait: a misaligned address, not yet"
    );
    let thread = smp::current()
        .active_thread()
        .expect("gsync_wait: no current thread");
    let remote = !ptr::eq(task, thread.task());
    // gsync_prepare_key, its map lock dropped before the words are read.
    let access = if flags.contains(Flags::MUTATE) {
        Protection::READ | Protection::WRITE
    } else {
        Protection::READ
    };
    assert!(
        task.map.probe(address, access).is_ok(),
        "gsync_wait: probe_address, KERN_INVALID_ADDRESS, not yet"
    );
    assert!(
        !flags.contains(Flags::SHARED),
        "gsync_prepare_key: GSYNC_SHARED, not yet"
    );
    let key = Key {
        map: Reference::as_ptr(&task.map).addr().get(),
        address,
    };
    assert!(!remote, "gsync_wait: temp_mapping, not yet");
    let bucket = key.bucket();
    let mut waiters = bucket.waiters.lock();
    // Read under the bucket's token, so no wake comes between the read and
    // the wait.
    let read = |address: VirtualAddress| {
        let mut word = [0; size_of::<u32>()];
        assert!(
            arch::Current::copy_in(usize::from(address), &mut word).is_ok(),
            "gsync_wait: copyin, KERN_INVALID_ADDRESS, not yet"
        );
        u32::from_ne_bytes(word)
    };
    let mut equal = read(address) == value;
    if flags.contains(Flags::QUAD) {
        let next = read(address + size_of::<u32>());
        equal = equal && next == value2;
    }
    if !equal {
        drop(waiters);
        return Err(Error::InvalidArgument);
    }
    let waiter = Waiter {
        link: LinkedListAtomicLink::new(),
        key,
        thread,
    };
    // Before the first greater key: after its key's earlier waiters.
    let mut cursor = waiters.front_mut();
    while cursor.get().is_some_and(|other| other.key <= waiter.key) {
        cursor.move_next();
    }
    // SAFETY: the waiter stays on this stack until the function returns,
    // and leaves the list before: a wake unlinks it before it ends the wait
    // as awakened, and any other end of the wait unlinks it below.
    cursor.insert_before(unsafe { UnsafeRef::from_raw(&raw const waiter) });
    if flags.contains(Flags::TIMED) {
        sched::will_wait_with_timeout(milliseconds);
    } else {
        sched::will_wait();
    }
    // A wake before the block would find the thread RUN|WAIT, which
    // `clear_wait` refuses: not yet (no waker runs here on this CPU).
    drop(waiters);
    sched::block();
    match thread.wait_result() {
        WaitResult::Awakened => Ok(()),
        WaitResult::TimedOut => {
            let mut waiters = bucket.waiters.lock();
            if waiter.link.is_linked() {
                // SAFETY: still linked, it is on its bucket's list, this one.
                unsafe { waiters.cursor_mut_from_ptr(&raw const waiter) }.remove();
            }
            drop(waiters);
            Err(Error::TimedOut)
        }
        WaitResult::Interrupted => panic!("gsync_wait: KERN_INTERRUPTED, not yet"),
    }
}

/// Stores `value` in the word at `address` in `task` with `MUTATE`, then
/// wakes the first thread waiting on that word, or with `BROADCAST` every
/// one, as `gsync_wake` does.
///
/// # Errors
///
/// `InvalidArgument` if no thread waits on the word.
///
/// # Panics
///
/// If no thread runs on the CPU; as `Map::probe` and `sched::clear_wait`.
/// For no task, a misaligned address, one `probe_address` refuses, a shared
/// word, a store to another task or one that fails: not yet.
pub fn wake(
    task: Option<&Task>,
    address: VirtualAddress,
    value: u32,
    flags: Flags,
) -> Result<(), Error> {
    let Some(task) = task else {
        panic!("gsync_wake: KERN_INVALID_TASK, not yet");
    };
    assert!(
        usize::from(address) % size_of::<u32>() == 0,
        "gsync_wake: a misaligned address, not yet"
    );
    // gsync_prepare_key, its map lock dropped before the store.
    let access = if flags.contains(Flags::MUTATE) {
        Protection::READ | Protection::WRITE
    } else {
        Protection::READ
    };
    assert!(
        task.map.probe(address, access).is_ok(),
        "gsync_wake: probe_address, KERN_INVALID_ADDRESS, not yet"
    );
    assert!(
        !flags.contains(Flags::SHARED),
        "gsync_prepare_key: GSYNC_SHARED, not yet"
    );
    let key = Key {
        map: Reference::as_ptr(&task.map).addr().get(),
        address,
    };
    let current = smp::current()
        .active_thread()
        .expect("gsync_wake: no current thread");
    let remote = !ptr::eq(task, current.task());
    assert!(
        !(remote && flags.contains(Flags::MUTATE)),
        "gsync_wake: temp_mapping, not yet"
    );
    let mut waiters = key.bucket().waiters.lock();
    // Stored under the bucket's token, so no wait reads the word between
    // the store and the wake.
    if flags.contains(Flags::MUTATE) {
        assert!(
            arch::Current::copy_out(&value.to_ne_bytes(), usize::from(address)).is_ok(),
            "gsync_wake: copyout, KERN_INVALID_ADDRESS, not yet"
        );
    }
    // The first waiter with the key: the list is in key order.
    let mut cursor = waiters.front_mut();
    while cursor.get().is_some_and(|waiter| waiter.key < key) {
        cursor.move_next();
    }
    if cursor.get().is_none_or(|waiter| waiter.key != key) {
        drop(waiters);
        return Err(Error::InvalidArgument);
    }
    loop {
        // Unlinked and read before the wake: a woken waiter's frame may go
        // at once.
        let thread = cursor.remove().unwrap().thread;
        sched::clear_wait(thread, WaitResult::Awakened);
        if !flags.contains(Flags::BROADCAST) || cursor.get().is_none_or(|waiter| waiter.key != key)
        {
            break;
        }
    }
    drop(waiters);
    Ok(())
}
