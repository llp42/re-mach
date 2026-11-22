// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The floating-point unit: x87, SSE and the states XSAVE adds.

pub mod context;
pub mod init;
pub mod save;
pub mod state;
