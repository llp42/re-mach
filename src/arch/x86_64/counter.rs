// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The time counter: an invariant TSC if the CPU has one, else the HPET.

use core::arch::x86_64::_rdtsc;

use super::{cpuid, hpet};
use crate::sync::OnceLock;

/// What counts time, and how fast.
enum Counter {
    /// The TSC, which runs at a constant rate in every state: its frequency,
    /// in hertz.
    Tsc(u64),
    /// The HPET's main counter: its period, in femtoseconds.
    Hpet(u64),
}

/// The counter, once `init` picks it.
static COUNTER: OnceLock<Counter> = OnceLock::new();

/// The span the TSC is measured over against the HPET, in femtoseconds:
/// 100 ms.
const MEASURE_SPAN: u64 = 100_000_000_000_000;

/// Picks the counter, on the boot CPU, once the HPET is started: an
/// invariant TSC, its frequency from CPUID or else measured against the
/// HPET, or else the HPET.
///
/// # Panics
///
/// If there is neither, or on a second call.
pub fn init() {
    let period = hpet::PERIOD.get().copied();
    let invariant = cpuid::cpu_id()
        .get_advanced_power_mgmt_info()
        .is_some_and(|info| info.has_invariant_tsc());
    let counter = match (invariant, period) {
        (true, _) if let Some(frequency) = tsc_frequency() => Counter::Tsc(frequency),
        (true, Some(period)) => Counter::Tsc(measure_tsc(period)),
        (false, Some(period)) => Counter::Hpet(period),
        (_, None) => panic!("counter: neither an invariant TSC nor an HPET"),
    };
    assert!(COUNTER.set(counter).is_ok(), "counter: picked twice");
}

/// The TSC's frequency as CPUID gives it: leaf 0x15's crystal and ratio,
/// else leaf 0x16's base frequency.
fn tsc_frequency() -> Option<u64> {
    let cpu_id = cpuid::cpu_id();
    cpu_id
        .get_tsc_info()
        .and_then(|info| info.tsc_frequency())
        .or_else(|| {
            cpu_id
                .get_processor_frequency_info()
                .map(|info| info.processor_base_frequency())
                .filter(|&megahertz| megahertz != 0)
                .map(|megahertz| u64::from(megahertz) * 1_000_000)
        })
}

/// The TSC's frequency, counted over `MEASURE_SPAN` of the HPET, whose
/// period is `period` femtoseconds.
fn measure_tsc(period: u64) -> u64 {
    let ticks = MEASURE_SPAN / period;
    let start = hpet::read();
    // SAFETY: RDTSC only reads the counter.
    let tsc_start = unsafe { _rdtsc() };
    while hpet::read() - start < ticks {}
    // SAFETY: as above.
    let tsc_end = unsafe { _rdtsc() };
    let span = u128::from(ticks) * u128::from(period);
    u64::try_from(u128::from(tsc_end - tsc_start) * 1_000_000_000_000_000 / span).unwrap()
}

/// Nanoseconds since the counter started.
///
/// # Panics
///
/// Before `init`.
pub fn nanoseconds() -> u64 {
    match COUNTER.get().expect("counter: none picked") {
        // SAFETY: RDTSC only reads the counter.
        Counter::Tsc(frequency) => {
            u64::try_from(u128::from(unsafe { _rdtsc() }) * 1_000_000_000 / u128::from(*frequency))
                .unwrap()
        }
        Counter::Hpet(period) => {
            u64::try_from(u128::from(hpet::read()) * u128::from(*period) / 1_000_000).unwrap()
        }
    }
}
