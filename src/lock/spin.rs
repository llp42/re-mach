// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Exclusive spinlocks, like dfly's `spin_lock` (`kern_spinlock.c`).

use core::hint;
use core::sync::atomic::{AtomicU32, Ordering};

use lock_api::{GuardNoSend, RawMutex};

/// One waiter in the lock word's high bits (dfly's `SPINLOCK_EXCLWAIT`); the
/// low bits count the holder and lockers on their way in.
const EXCLUSIVE_WAIT: u32 = 0x0010_0000;
/// Longest backoff, in `pause`s (dfly's `6 + spin_backoff_max`).
const BACKOFF_MAX: u32 = 6 + 4096;

/// dfly's `struct spinlock`, exclusive mode only.
pub struct RawSpinLock {
    lock: AtomicU32,
}

impl RawSpinLock {
    /// Waits after the fast path found the lock taken, as dfly's
    /// `_spin_lock_contested`: our count moves to the high bits, so no new
    /// locker gets ahead, until the low bits drop to zero.
    #[cold]
    fn lock_contested(&self) {
        self.lock.fetch_add(EXCLUSIVE_WAIT - 1, Ordering::Relaxed);
        // dfly's n^1.5 backoff: 1, 3, 6, 10, ...
        let mut backoff = 0;
        loop {
            backoff = ((backoff + 1) * 3 / 2).min(BACKOFF_MAX);
            for _ in 0..backoff {
                hint::spin_loop();
            }
            let value = self.lock.load(Ordering::Relaxed);
            if value & (EXCLUSIVE_WAIT - 1) == 0
                && self
                    .lock
                    .compare_exchange(
                        value,
                        (value - EXCLUSIVE_WAIT) | 1,
                        Ordering::Acquire,
                        Ordering::Relaxed,
                    )
                    .is_ok()
            {
                return;
            }
        }
    }
}

// SAFETY: `lock` returns and `try_lock` succeeds only once the low bits went
// from 0 to 1, which one locker at a time can do, until `unlock`.
unsafe impl RawMutex for RawSpinLock {
    const INIT: Self = Self {
        lock: AtomicU32::new(0),
    };

    /// The guard stays on its thread, as dfly's critical section would.
    type GuardMarker = GuardNoSend;

    /// dfly's `spin_lock`.
    fn lock(&self) {
        if self.lock.fetch_add(1, Ordering::Acquire) != 0 {
            self.lock_contested();
        }
    }

    /// dfly's `spin_trylock`.
    fn try_lock(&self) -> bool {
        self.lock
            .compare_exchange(0, 1, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
    }

    /// dfly's `spin_unlock`.
    unsafe fn unlock(&self) {
        self.lock.fetch_sub(1, Ordering::Release);
    }
}

/// A `T` behind a `RawSpinLock`.
pub type SpinLock<T> = lock_api::Mutex<RawSpinLock, T>;
