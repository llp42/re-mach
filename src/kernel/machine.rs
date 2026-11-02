// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Machine slots: what the host knows of each processor.

use crate::config::MAX_CPUS;
use crate::sync::OnceLock;

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

/// What the host knows of a processor slot that holds one.
#[expect(dead_code, reason = "host_info and processor_info read it")]
pub struct Slot {
    pub ty: CpuType,
    pub subtype: CpuSubtype,
}

/// One per processor, set once it is found.
static SLOTS: [OnceLock<Slot>; MAX_CPUS] = [const { OnceLock::new() }; MAX_CPUS];

/// Stores processor `cpu`'s slot.
///
/// # Panics
///
/// If it is stored already.
pub fn store_slot(cpu: usize, slot: Slot) {
    assert!(
        SLOTS[cpu].set(slot).is_ok(),
        "machine: slot {cpu} stored twice"
    );
}
