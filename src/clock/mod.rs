// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The clock: the time of day and timeouts.

pub mod mapped_time;
pub mod softclock;
pub mod time;
pub mod timeout;

use core::sync::atomic::Ordering;

use mapped_time::MAPPED_TIME;
use time::{publish, snapshot};

use crate::arch::{Current, Machine as _};
use crate::machine::{self, CpuState, Mode};
use crate::processor::State;
use crate::sched::{self, WaitResult};
use crate::smp;

/// Takes a clock tick on the current CPU, `microseconds` after the one
/// before, with the thread it interrupted running in `mode`: the tick is
/// the thread's, and the CPU's in that mode, or idle; it advances the
/// CPU's timeouts, and on the boot CPU the time of day and the uptime, then
/// wakes the softclock thread if it is not running and a timeout is due or
/// the wheel is behind.
pub fn interrupt(microseconds: u32, mode: Mode) {
    let cpu = smp::current();
    let thread = cpu.active_thread();
    if let Some(thread) = thread {
        thread.add_time(microseconds, mode);
    }
    let state = match mode {
        Mode::User => CpuState::User,
        Mode::Kernel if matches!(cpu.processor().state(), State::Idle) => CpuState::Idle,
        Mode::Kernel => CpuState::System,
    };
    machine::count_tick(cpu.id(), state);
    if let Some(thread) = thread {
        sched::quantum_update(thread, 1, state);
    }
    let wheel = cpu.timeouts();
    let ticks = wheel.ticks.load(Ordering::Relaxed) + 1;
    wheel.ticks.store(ticks, Ordering::Relaxed);
    // The softclock thread runs the timeouts due, and checks the ticks
    // itself while it runs; else a tick with none due, and none behind, is
    // checked at once, or the thread is woken.
    let wake = if wheel.running.load(Ordering::Relaxed) {
        false
    } else {
        let slot = wheel.slot(ticks).lock();
        let checked = slot.is_empty() && wheel.checked_ticks.load(Ordering::Relaxed) + 1 == ticks;
        if checked {
            wheel.checked_ticks.store(ticks, Ordering::Relaxed);
        } else {
            wheel.running.store(true, Ordering::Relaxed);
        }
        drop(slot);
        !checked
    };
    if cpu.id() == 0 {
        let nanoseconds = i64::from(microseconds) * 1000;
        let last = snapshot();
        let time = last.time.load().add_nanoseconds(nanoseconds);
        let uptime = last.uptime.load().add_nanoseconds(nanoseconds);
        publish(time, uptime, Current::counter_nanoseconds());
        if let Some(mapped) = MAPPED_TIME.get() {
            mapped.set_time(time);
            mapped.set_uptime(uptime);
        }
    }
    // One made later finds the wheel behind as it first runs.
    if wake && let Some(&thread) = wheel.thread.get() {
        sched::clear_wait(thread, WaitResult::Awakened);
    }
}
