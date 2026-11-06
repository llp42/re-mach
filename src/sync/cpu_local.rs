// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data, touched inside a critical section: a slot per CPU, which
//! only that CPU touches, or data a CPU owns, which another touches only
//! before that CPU starts.

use core::mem::MaybeUninit;
use core::ops::{Deref, DerefMut};
use core::ptr;
use core::sync::atomic::{AtomicBool, Ordering};

use lock_api::{GuardNoSend, MutexGuard, RawMutex};

use super::CachePadded;
use super::critical_section::{CriticalSection, crit_enter, crit_exit};
use crate::config::MAX_CPUS;
use crate::smp;

/// A slot's raw lock: a critical section, and a flag that catches its CPU
/// taking it twice.
pub struct RawCpuLock {
    held: AtomicBool,
}

// SAFETY: it is taken inside a critical section, by `CpuLocal::lock` on the
// current CPU's slot, or as `CpuLock::new`'s caller promises, so no other
// CPU and no interrupt reaches it; the flag stops the one case left, its CPU
// taking it twice.
unsafe impl RawMutex for RawCpuLock {
    const INIT: Self = Self {
        held: AtomicBool::new(false),
    };

    type GuardMarker = GuardNoSend;

    fn lock(&self) {
        assert!(self.try_lock(), "per-CPU data locked twice");
    }

    fn try_lock(&self) -> bool {
        crit_enter();
        if self.held.load(Ordering::Relaxed) {
            crit_exit();
            return false;
        }
        self.held.store(true, Ordering::Relaxed);
        true
    }

    unsafe fn unlock(&self) {
        self.held.store(false, Ordering::Relaxed);
        crit_exit();
    }
}

/// Data its CPU locks, inside a critical section; another CPU only before
/// that one starts.
pub struct CpuLock<T>(lock_api::Mutex<RawCpuLock, T>);

impl<T> CpuLock<T> {
    /// One holding `value`.
    ///
    /// # Safety
    ///
    /// Only its CPU locks it, or another CPU before that one starts.
    pub const unsafe fn new(value: T) -> Self {
        Self(lock_api::Mutex::new(value))
    }

    /// Locks it, inside a critical section.
    ///
    /// # Panics
    ///
    /// If this CPU holds it already.
    pub fn lock(&self) -> MutexGuard<'_, RawCpuLock, T> {
        self.0.lock()
    }
}

/// One CPU's slot.
type Slot<T> = lock_api::Mutex<RawCpuLock, T>;

/// Data with a slot per CPU, each reached only by its own CPU, and each on
/// its own cache lines.
pub struct CpuLocal<T>([CachePadded<Slot<T>>; MAX_CPUS]);

impl<T: Copy> CpuLocal<T> {
    /// Every CPU's slot holding `value`.
    pub const fn new(value: T) -> Self {
        let mut slots = [const { MaybeUninit::<CachePadded<Slot<T>>>::uninit() }; MAX_CPUS];
        let mut cpu = 0;
        while cpu < MAX_CPUS {
            slots[cpu] = MaybeUninit::new(CachePadded::new(Slot::new(value)));
            cpu += 1;
        }
        // SAFETY: the loop wrote every slot.
        Self(unsafe {
            ptr::from_ref(&slots)
                .cast::<[CachePadded<Slot<T>>; MAX_CPUS]>()
                .read()
        })
    }
}

impl<T> CpuLocal<T> {
    /// Locks the current CPU's slot, inside a critical section.
    ///
    /// # Panics
    ///
    /// If this CPU holds it already.
    pub fn lock(&self) -> CpuGuard<'_, T> {
        // The section keeps the thread on this CPU until the slot's lock
        // enters its own.
        let _section = CriticalSection::enter();
        CpuGuard(self.0[smp::current().id()].lock())
    }
}

/// The current CPU's slot, locked; it leaves no way to another CPU's.
pub struct CpuGuard<'a, T>(MutexGuard<'a, RawCpuLock, T>);

impl<T> Deref for CpuGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T> DerefMut for CpuGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.0
    }
}
