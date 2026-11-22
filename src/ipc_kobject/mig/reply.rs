// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::ipc::kernel_message::header::Header;

use super::argument::Argument;

/// MIG's reply header, `mig_reply_header_t`.
#[repr(C)]
pub struct ReplyHeader {
    /// The reply's header.
    pub header: Header,
    /// The routine's return code.
    pub return_code: Argument<u32>,
}
