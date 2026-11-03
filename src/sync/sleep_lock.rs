// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Sleep locks: readers share them, a writer holds them alone, and a thread
//! that must wait for one sleeps; they enter no critical section.

use core::sync::atomic::{AtomicUsize, Ordering};

use lock_api::{GuardNoSend, RawRwLock};

/// The state's bit for a writer; the bits below count readers.
const WRITER: usize = 1 << (usize::BITS - 1);

/// A sleep lock's raw lock.
pub struct RawSleepLock {
    state: AtomicUsize,
}

// SAFETY: a reader enters only while no writer holds it, and a writer only
// while nobody does, each by an acquire read-modify-write; unlocking undoes
// its own part with a release.
unsafe impl RawRwLock for RawSleepLock {
    const INIT: Self = Self {
        state: AtomicUsize::new(0),
    };

    type GuardMarker = GuardNoSend;

    fn lock_shared(&self) {
        // Waiting comes with threads; until then nothing can wait.
        assert!(self.try_lock_shared(), "sleep lock: would sleep");
    }

    fn try_lock_shared(&self) -> bool {
        self.state
            .try_update(Ordering::Acquire, Ordering::Relaxed, |state| {
                (state & WRITER == 0).then_some(state + 1)
            })
            .is_ok()
    }

    unsafe fn unlock_shared(&self) {
        self.state.fetch_sub(1, Ordering::Release);
    }

    fn lock_exclusive(&self) {
        // Waiting comes with threads; until then nothing can wait.
        assert!(self.try_lock_exclusive(), "sleep lock: would sleep");
    }

    fn try_lock_exclusive(&self) -> bool {
        self.state
            .compare_exchange(0, WRITER, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    unsafe fn unlock_exclusive(&self) {
        self.state.store(0, Ordering::Release);
    }
}

/// A lock readers share and a writer holds alone, whose waiters sleep.
pub type SleepLock<T> = lock_api::RwLock<RawSleepLock, T>;
