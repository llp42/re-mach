// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Kernel parameters, like dfly's `subr_param.c`: for now the timekeeping
//! half of `init_param1`; Mach has no BSD process size limits.

use core::sync::atomic::{AtomicI32, Ordering};

/// Clock interrupts per second by default (dfly's `HZ_DEFAULT`).
const HZ_DEFAULT: i32 = 100;

/// Clock interrupts per second (`hz`).
pub static HZ: AtomicI32 = AtomicI32::new(0);
/// Statistics clock rate (`stathz`).
pub static STAT_HZ: AtomicI32 = AtomicI32::new(0);
/// Profiling clock rate (`profhz`).
pub static PROFILE_HZ: AtomicI32 = AtomicI32::new(0);
/// A tick in microseconds (`ustick`).
pub static TICK_MICROSECONDS: AtomicI32 = AtomicI32::new(0);
/// A tick in nanoseconds (`nstick`).
pub static TICK_NANOSECONDS: AtomicI32 = AtomicI32::new(0);
/// The NTP adjustment per tick, in nanoseconds (`ntp_default_tick_delta`,
/// from `kern_clock.c`).
pub static NTP_DEFAULT_TICK_DELTA: AtomicI32 = AtomicI32::new(0);

/// dfly's `init_param1`, the timekeeping part; its tunables are left out,
/// as there is no loader environment.
pub fn init1() {
    let hz = HZ_DEFAULT;
    HZ.store(hz, Ordering::Relaxed);
    let stat_hz = hz + 1;
    STAT_HZ.store(stat_hz, Ordering::Relaxed);
    PROFILE_HZ.store(stat_hz, Ordering::Relaxed);
    TICK_MICROSECONDS.store(1_000_000 / hz, Ordering::Relaxed);
    TICK_NANOSECONDS.store(1_000_000_000 / hz, Ordering::Relaxed);
    // Can adjust 30 ms in 60 s (dfly's `howmany`, rounding up).
    NTP_DEFAULT_TICK_DELTA.store((30_000_000 + 60 * hz - 1) / (60 * hz), Ordering::Relaxed);
}
