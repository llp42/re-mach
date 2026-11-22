// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The scheduler.

pub mod event;
pub mod load;
pub mod policy;
pub mod preemption;
pub mod priority;
pub mod queue;
pub mod switch;
pub mod tick;
pub mod wait;
