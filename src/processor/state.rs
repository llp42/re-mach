// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

/// What a processor is doing, as Mach numbers it; only the states used
/// here.
pub enum State {
    /// Not in the system.
    Offline,
    /// Running a thread.
    Running,
    /// Running its idle thread.
    Idle,
}

impl State {
    /// Its number.
    pub(crate) const fn number(self) -> u8 {
        match self {
            Self::Offline => 0,
            Self::Running => 1,
            Self::Idle => 2,
        }
    }
}
