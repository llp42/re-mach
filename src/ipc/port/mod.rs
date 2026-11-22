// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Ports.

pub mod allocate;
pub mod kernel_object;
#[expect(clippy::module_inception, reason = "the file is named for its type")]
pub mod port;
pub mod port_name;
pub mod reply;
pub mod rights;
pub mod send_right;
pub mod state;
