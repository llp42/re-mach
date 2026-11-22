// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

bitflags::bitflags! {
    /// What a thread is doing, as Mach numbers it; only the states used
    /// here.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub struct ThreadState: u32 {
        /// Waiting for an event.
        const WAIT = 0x01;
        /// Asked to stop.
        const SUSPENDED = 0x02;
        /// Running, or waiting to run.
        const RUN = 0x04;
        /// Its wait may not be interrupted.
        const UNINTERRUPTIBLE = 0x08;
        /// Stopped at a clean point, on its way out of the kernel.
        const HALTED = 0x10;
        /// A processor's idle thread.
        const IDLE = 0x80;
    }
}

impl ThreadState {
    /// The states that say whether it may run.
    pub(crate) const SCHEDULING: Self = Self::WAIT
        .union(Self::SUSPENDED)
        .union(Self::RUN)
        .union(Self::UNINTERRUPTIBLE);
}

/// Whether a thread's quantum ran out at a tick.
#[derive(Clone, Copy)]
pub enum Quantum {
    /// It ran out.
    Expired,
    /// Some of it is left.
    Left,
}

/// What a thread is doing, as `thread_info` reports it.
#[derive(Clone, Copy)]
pub(super) enum RunState {
    /// Running, or waiting to run.
    Running,
    /// Stopped.
    Stopped,
    /// Waiting for an event.
    Waiting,
    /// Waiting uninterruptibly.
    Uninterruptible,
    /// Halted at a clean point.
    Halted,
}

impl TryFrom<ThreadState> for RunState {
    type Error = ThreadState;

    /// The run state `state` shows, in Mach's order; none if it shows none.
    fn try_from(state: ThreadState) -> Result<Self, ThreadState> {
        if state.contains(ThreadState::HALTED) {
            Ok(Self::Halted)
        } else if state.contains(ThreadState::RUN) {
            Ok(Self::Running)
        } else if state.contains(ThreadState::UNINTERRUPTIBLE) {
            Ok(Self::Uninterruptible)
        } else if state.contains(ThreadState::SUSPENDED) {
            Ok(Self::Stopped)
        } else if state.contains(ThreadState::WAIT) {
            Ok(Self::Waiting)
        } else {
            Err(state)
        }
    }
}

/// Mach's `TH_STATE_*` number.
impl From<RunState> for i32 {
    fn from(state: RunState) -> Self {
        match state {
            RunState::Running => 1,
            RunState::Stopped => 2,
            RunState::Waiting => 3,
            RunState::Uninterruptible => 4,
            RunState::Halted => 5,
        }
    }
}
