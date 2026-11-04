// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Device registers mapped into the kernel map.

use core::ptr::NonNull;

use super::address::PhysAddr;
use crate::kernel::Error;
use crate::vm;

/// A device's 32-bit registers, mapped uncached.
pub struct Registers(NonNull<u32>);

// SAFETY: every access is one volatile read or write of a whole register;
// what an access means is the device's.
unsafe impl Send for Registers {}
// SAFETY: as above.
unsafe impl Sync for Registers {}

impl Registers {
    /// Maps the `size` bytes of registers at `physical`.
    ///
    /// # Errors
    ///
    /// As `vm::kernel::map_device`.
    pub fn map(physical: PhysAddr, size: usize) -> Result<Self, Error> {
        vm::kernel::map_device(physical, size).map(|base| Self(base.cast()))
    }

    /// Where they are mapped.
    pub fn address(&self) -> usize {
        self.0.addr().get()
    }

    /// Reads the register at byte `offset`.
    pub fn read(&self, offset: usize) -> u32 {
        // SAFETY: `offset` is inside the mapped registers, 4-byte aligned.
        unsafe { self.0.byte_add(offset).read_volatile() }
    }

    /// Writes `value` to the register at byte `offset`.
    pub fn write(&self, offset: usize, value: u32) {
        // SAFETY: as in `read`.
        unsafe { self.0.byte_add(offset).write_volatile(value) }
    }
}
