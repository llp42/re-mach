// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Machine slots: what the host knows of each processor.

use core::cell::UnsafeCell;

use crate::config::MAX_CPUS;

/// A processor type, as Mach's interface numbers it; only those used here.
#[repr(transparent)]
pub struct CpuType(pub i32);

impl CpuType {
    pub const X86_64: Self = Self(21);
}

/// A processor subtype, as Mach's interface numbers it; only those used
/// here.
#[repr(transparent)]
pub struct CpuSubtype(pub i32);

impl CpuSubtype {
    pub const AT386: Self = Self(1);
}

/// What the host knows of one processor slot.
#[expect(dead_code, reason = "host_info and processor_info read it")]
pub struct MachineSlot {
    /// Whether a processor sits in it.
    pub is_cpu: bool,
    pub cpu_type: CpuType,
    pub cpu_subtype: CpuSubtype,
}

struct MachineSlots(UnsafeCell<[MachineSlot; MAX_CPUS]>);

// SAFETY: written on the boot CPU, by `store_slot`, before any reader.
unsafe impl Sync for MachineSlots {}

/// One per processor; empty, with no type, until stored.
static MACHINE_SLOTS: MachineSlots = MachineSlots(UnsafeCell::new(
    [const {
        MachineSlot {
            is_cpu: false,
            cpu_type: CpuType(0),
            cpu_subtype: CpuSubtype(0),
        }
    }; MAX_CPUS],
));

/// Stores processor `cpu`'s slot.
///
/// # Safety
///
/// Runs on the boot CPU, before any reader.
pub unsafe fn store_slot(cpu: usize, slot: MachineSlot) {
    // SAFETY: guaranteed by the caller.
    unsafe { (*MACHINE_SLOTS.0.get())[cpu] = slot };
}
