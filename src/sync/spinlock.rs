// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Spinlocks: held inside a critical section, so their holder's CPU takes
//! no interrupt until it releases them.

use core::hint;
use core::sync::atomic::{AtomicBool, Ordering};

use lock_api::{GuardNoSend, RawMutex};

use super::critical_section::{crit_enter, crit_exit};

/// A spinlock's raw lock: a flag, taken inside a critical section.
pub struct RawSpinlock {
    locked: AtomicBool,
}

// SAFETY: the flag is taken by an acquire compare-and-swap, so it has one
// holder at a time, and `unlock` releases it.
unsafe impl RawMutex for RawSpinlock {
    const INIT: Self = Self {
        locked: AtomicBool::new(false),
    };

    type GuardMarker = GuardNoSend;

    fn lock(&self) {
        crit_enter();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                hint::spin_loop();
            }
        }
    }

    fn try_lock(&self) -> bool {
        crit_enter();
        let taken = self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();
        if !taken {
            crit_exit();
        }
        taken
    }

    unsafe fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
        crit_exit();
    }
}

/// A lock that spins, held inside a critical section on its holder's CPU.
pub type Spinlock<T> = lock_api::Mutex<RawSpinlock, T>;
