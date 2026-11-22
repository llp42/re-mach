// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr;
use core::sync::atomic::{AtomicI64, Ordering};

use crate::config::MAX_CPUS;
use crate::processor::{self, state::State};
use crate::sched::policy::{SCHEDULER_SCALE, SCHEDULER_SHIFT};
use crate::smp;

/// What Mach factors and load averages are scaled by.
const LOAD_SCALE: i64 = 1000;

/// How much of its last value each system average keeps, over
/// `LOAD_SCALE`: they span 5 seconds, 30 seconds and a minute.
const DECAY: [i64; 3] = [800, 966, 983];

/// The default set's load averages over those spans, by `LOAD_SCALE`;
/// written only by `compute_mach_factor`.
static AVENRUN: [AtomicI64; 3] = [const { AtomicI64::new(0) }; 3];

/// The default set's Mach factors over those spans, by `LOAD_SCALE`;
/// written only by `compute_mach_factor`.
static MACH_FACTOR: [AtomicI64; 3] = [const { AtomicI64::new(0) }; 3];

/// The default set's load averages, by `LOAD_SCALE`.
pub fn load_averages() -> [i64; 3] {
    AVENRUN
        .each_ref()
        .map(|average| average.load(Ordering::Relaxed))
}

/// The default set's Mach factors, by `LOAD_SCALE`.
pub fn mach_factors() -> [i64; 3] {
    MACH_FACTOR
        .each_ref()
        .map(|factor| factor.load(Ordering::Relaxed))
}

/// Computes each processor set's Mach factor and loads, under the sets'
/// token and its own, from the threads its processors queue and run, the
/// current one aside; the default set's feed the system averages. A set
/// with no processor has none.
pub fn compute_mach_factor() {
    let all = processor::all_sets::ALL_SETS.lock();
    for set in &all.list {
        let members = set.lock();
        let count = i64::from(set.processor_count());
        if count > 0 {
            // Each processor's run queues hold the set's threads too.
            let (queued, idle) = (0..MAX_CPUS)
                .map(|cpu| smp::of(cpu).processor())
                .filter(|processor| processor.set().is_some_and(|its| ptr::eq(its, set)))
                .fold((0, 0), |(queued, idle), processor| {
                    (
                        queued + i64::from(processor.queued()),
                        idle + i64::from(matches!(processor.state(), State::Idle)),
                    )
                });
            let is_default = ptr::eq(set, &raw const processor::all_sets::DEFAULT_SET);
            // Less the current thread, in the default set.
            let threads = queued + count - idle - i64::from(is_default);
            let (factor, load) = if threads > count {
                (
                    count * LOAD_SCALE / (threads + 1),
                    (threads << SCHEDULER_SHIFT) / count,
                )
            } else {
                ((count - threads) * LOAD_SCALE, i64::from(SCHEDULER_SCALE))
            };
            let average = threads * LOAD_SCALE;
            set.update_averages(factor, average, u32::try_from(load).unwrap());
            if is_default {
                for (decay, (mach_factor, avenrun)) in
                    DECAY.into_iter().zip(MACH_FACTOR.iter().zip(&AVENRUN))
                {
                    let decayed = |old: i64, now: i64| {
                        (old * decay + now * (LOAD_SCALE - decay)) / LOAD_SCALE
                    };
                    mach_factor.store(
                        decayed(mach_factor.load(Ordering::Relaxed), factor),
                        Ordering::Relaxed,
                    );
                    avenrun.store(
                        decayed(avenrun.load(Ordering::Relaxed), average),
                        Ordering::Relaxed,
                    );
                }
            }
        }
        drop(members);
    }
    drop(all);
}
