// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::ipc::MessageType;

use super::argument::{Inline, word};

/// A device's name, MIG's `dev_name_t`: a C string of 128 bytes, in one
/// long-form item.
#[derive(Clone, Copy)]
#[repr(transparent)]
pub(in crate::ipc_kobject) struct DeviceName([u8; 128]);

impl DeviceName {
    /// Its bytes before the NUL MIG's stub puts at its last byte.
    pub(in crate::ipc_kobject) fn as_bytes(&self) -> &[u8] {
        let bytes = &self.0[..127];
        &bytes[..bytes
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(bytes.len())]
    }
}

// SAFETY: bytes only.
unsafe impl Inline for DeviceName {
    // One item of 1024 bits, inline and long-form.
    const WORD: u64 = word(MessageType::StringC, 1024, (1 << 5) | (1 << 6), 1);
    // All but msgt_deallocate and msgt_unused, as _Xdevice_open.
    const CHECKED: u64 = !((1 << 31) | (0x1f << 24));
}
