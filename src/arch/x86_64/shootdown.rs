// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! TLB shootdowns: each sender's request, and the interrupt that serves it.

use core::sync::atomic::{AtomicUsize, Ordering};

use x86_64::VirtAddr;
use x86_64::instructions::tlb;

use super::memory_layout::PAGE_SIZE;
use crate::config::MAX_CPUS;
use crate::smp::{self, CpuMask};

/// The shootdown interrupt's vector.
pub const VECTOR: usize = 0xfb;

/// Most pages invalidated one by one; a longer range flushes the TLB.
const MAX_PAGES: usize = 128;

/// A sender's request: a range of addresses, and the CPUs that have yet to
/// invalidate it.
#[repr(align(64))]
struct Request {
    /// Where the range starts.
    start: AtomicUsize,
    /// Where the range ends.
    end: AtomicUsize,
    /// The CPUs that have yet to invalidate it; the sender sets them after
    /// the range, each clears its own after invalidating.
    pending: CpuMask,
}

/// Each CPU's request as a sender, by CPU number, a cache line each.
static REQUESTS: [Request; MAX_CPUS] = [const {
    Request {
        start: AtomicUsize::new(0),
        end: AtomicUsize::new(0),
        pending: CpuMask::empty(),
    }
}; MAX_CPUS];

/// The CPUs whose request is in flight.
static SENDERS: CpuMask = CpuMask::empty();

/// Serves every request pending on the current CPU, with interrupts off; it
/// is never held back, so it takes no lock and no `CpuLocal`. Scanning every
/// sender serves requests whose interrupts arrived as one.
pub fn interrupt() {
    let cpu = smp::current().id();
    for sender in SENDERS.iter() {
        let request = &REQUESTS[sender];
        // Acquire: the range, stored before `pending`, is seen.
        if !request.pending.iter().any(|target| target == cpu) {
            continue;
        }
        let range = request.start.load(Ordering::Relaxed)..request.end.load(Ordering::Relaxed);
        if (range.end - range.start).div_ceil(PAGE_SIZE) > MAX_PAGES {
            // Removable kernel PTEs are not global, so reloading CR3 drops
            // them.
            tlb::flush_all();
        } else {
            for page in range.step_by(PAGE_SIZE) {
                tlb::flush(VirtAddr::new(u64::try_from(page).unwrap()));
            }
        }
        // A locked RMW, after the invalidation: the sender's wait sees it
        // done.
        request.pending.remove(cpu);
    }
}
