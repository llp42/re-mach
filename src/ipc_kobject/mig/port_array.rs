// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;

use crate::ipc::MessageType;

use super::argument::word;

/// An out-of-line array of send rights a routine gives: kalloc memory of
/// its ports' exposed addresses, each holding its right's reference; none
/// for no ports.
pub(in crate::ipc_kobject) struct PortArray(Option<NonNull<[usize]>>);

impl PortArray {
    /// The send rights in `ports`.
    ///
    /// # Safety
    ///
    /// `ports`, if any, came from `kalloc::allocate` of its size, and holds
    /// the exposed addresses of references to ports given up by `into_raw`,
    /// which it takes.
    pub(in crate::ipc_kobject) const unsafe fn new(ports: Option<NonNull<[usize]>>) -> Self {
        Self(ports)
    }

    /// Its type word, and the array's address, which the reply takes.
    ///
    /// # Panics
    ///
    /// If the count is past 32 bits.
    pub(in crate::ipc_kobject) fn into_parts(self) -> (u64, usize) {
        let count = u32::try_from(self.0.map_or(0, NonNull::len)).unwrap();
        // Send rights of 64 bits, out of line, in the long form, as
        // _Xtask_threads.
        (
            word(MessageType::PortSend, 64, 1 << 6, count),
            self.0.map_or(0, |ports| {
                ports.cast::<usize>().as_ptr().expose_provenance()
            }),
        )
    }
}

/// The bits of an out-of-line item's or an inline array's type word MIG's
/// stub checks: all but the number, `msgt_deallocate` and `msgt_unused`, as
/// `_Xvm_write` and `_Xthread_set_state`.
pub(in crate::ipc_kobject) const ARRAY_CHECKED: u64 =
    !((0xffff_ffff << 32) | (1 << 31) | (0x1f << 24));
