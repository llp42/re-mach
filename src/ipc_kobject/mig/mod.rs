// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! MIG's server stubs, generated from a subsystem's routines.

pub mod argument;
pub mod boolean;
pub mod device_name;
pub mod port_array;
pub mod reply;
pub mod string_char;
pub mod subsystem;

pub(crate) use subsystem::subsystem;
