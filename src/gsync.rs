// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Global synchronisation: threads that wait on a word of memory, hashed
//! into buckets by where the word is; so far only the empty buckets.

use intrusive_collections::{LinkedList, LinkedListAtomicLink, UnsafeRef, intrusive_adapter};

/// A thread waiting on a word; so far only its place in a bucket.
pub struct Waiter {
    /// Its place in its bucket.
    link: LinkedListAtomicLink,
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
    /// The waiters; their guard comes with `gsync_wait`.
    #[expect(dead_code, reason = "gsync_wait and gsync_wake use them")]
    waiters: LinkedList<WaiterAdapter>,
}

/// How many buckets there are.
const BUCKET_COUNT: usize = 512;

/// The buckets, each with no waiter.
#[expect(dead_code, reason = "gsync_wait and gsync_wake use them")]
static BUCKETS: [Bucket; BUCKET_COUNT] = [const {
    Bucket {
        waiters: LinkedList::new(WaiterAdapter::NEW),
    }
}; BUCKET_COUNT];
