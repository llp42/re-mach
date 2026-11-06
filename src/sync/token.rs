// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Tokens: a thread holds one while it runs, gives it up when it blocks and
//! takes it back before it runs again, so tokens never deadlock and may be
//! held across a sleep. They enter no critical section, and interrupt
//! handlers never take them.

use core::sync::atomic::{AtomicBool, Ordering};

use lock_api::{GuardNoSend, RawMutex};

use crate::smp;

/// A token's raw lock.
pub struct RawToken {
    held: AtomicBool,
}

// SAFETY: the flag is taken by an acquire compare-and-swap, so it has one
// holder at a time, and `unlock` releases it.
unsafe impl RawMutex for RawToken {
    const INIT: Self = Self {
        held: AtomicBool::new(false),
    };

    type GuardMarker = GuardNoSend;

    fn lock(&self) {
        // Waiting, and giving tokens up on blocking, come with threads;
        // until then nothing can wait.
        assert!(self.try_lock(), "token: would wait");
    }

    fn try_lock(&self) -> bool {
        let taken = self
            .held
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_ok();
        if taken {
            smp::current().take_token();
        }
        taken
    }

    unsafe fn unlock(&self) {
        self.held.store(false, Ordering::Release);
        smp::current().release_token();
    }
}

/// A lock its holder keeps while it runs, and gives up while it sleeps.
pub type Token<T> = lock_api::Mutex<RawToken, T>;
