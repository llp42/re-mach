// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! TLB shootdowns: each sender's request, and the interrupt that serves it.

use core::hint;
use core::ops::Range;
use core::sync::atomic::{AtomicUsize, Ordering};

use x86_64::VirtAddr;
use x86_64::instructions::{interrupts, tlb};

use super::apic;
use super::memory_layout::PAGE_SIZE;
use crate::config::MAX_CPUS;
use crate::smp::{self, CpuMask};
use crate::sync::CriticalSection;

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

/// Invalidates `range`, changed in the kernel's page tables, on every CPU
/// that uses them, once the caller's atomic swaps are done: it interrupts
/// the other started CPUs and waits for them. With interrupts on, so
/// another sender's request still lands on this CPU while it waits.
///
/// # Panics
///
/// With interrupts off.
pub fn shoot(range: Range<usize>) {
    let _section = CriticalSection::enter();
    assert!(
        interrupts::are_enabled(),
        "PMAP_UPDATE_TLBS: interrupts off"
    );
    let current = smp::current().id();
    // The other started CPUs. Acquire loads after the caller's swaps,
    // locked RMWs they can't pass: a CPU joining later flushes after its
    // SeqCst join.
    let mut targets = smp::STARTED.iter().filter(|&cpu| cpu != current).peekable();
    if targets.peek().is_none() {
        invalidate(range);
        return;
    }
    let request = &REQUESTS[current];
    // Relaxed: `pending`'s RMWs below publish them.
    request.start.store(range.start, Ordering::Relaxed);
    request.end.store(range.end, Ordering::Relaxed);
    // SeqCst RMWs, after the range: pairs with the receiver's Acquire load.
    for target in targets {
        request.pending.insert(target);
    }
    // SeqCst RMW, after `pending`: pairs with the receiver's Acquire scan.
    SENDERS.insert(current);
    // Acquire; a target that already served it, on another's interrupt,
    // needs none.
    for target in request.pending.iter() {
        apic::send_ipi(target, u8::try_from(VECTOR).unwrap());
    }
    invalidate(range);
    // Acquire: pairs with each target's clear, after its invalidation.
    while request.pending.iter().next().is_some() {
        hint::spin_loop();
    }
    // SeqCst RMW: receivers stop scanning it.
    SENDERS.remove(current);
}

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
        invalidate(request.start.load(Ordering::Relaxed)..request.end.load(Ordering::Relaxed));
        // A locked RMW, after the invalidation: the sender's wait sees it
        // done.
        request.pending.remove(cpu);
    }
}

/// Invalidates `range` in the current CPU's TLB: page by page, or all of
/// it for a longer range.
fn invalidate(range: Range<usize>) {
    if (range.end - range.start).div_ceil(PAGE_SIZE) > MAX_PAGES {
        // Removable kernel PTEs are not global, so reloading CR3 drops
        // them.
        tlb::flush_all();
    } else {
        for page in range.step_by(PAGE_SIZE) {
            tlb::flush(VirtAddr::new(u64::try_from(page).unwrap()));
        }
    }
}
