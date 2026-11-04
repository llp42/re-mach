// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Machine slots: what the host knows of each processor.

use crate::config::MAX_CPUS;
use crate::sync::OnceLock;
use crate::vm;

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

/// What the host knows of the machine, as Mach's interface gives it; the
/// CPUs available come with them.
#[expect(dead_code, reason = "host_info reads it")]
pub struct MachineInfo {
    /// The kernel's major version.
    major_version: i32,
    /// The kernel's minor version.
    minor_version: i32,
    /// The most CPUs the kernel supports.
    max_cpus: i32,
    /// The bytes of memory, holes within its segments included.
    memory_size: usize,
}

/// The kernel's version, as Mach's interface numbers it.
const MAJOR_VERSION: i32 = 4;
const MINOR_VERSION: i32 = 0;

/// The machine's information, once `init_info` fills it in.
static INFO: OnceLock<MachineInfo> = OnceLock::new();

/// Fills in the machine's information, on the boot CPU.
///
/// # Panics
///
/// On a second call.
pub fn init_info() {
    let info = MachineInfo {
        major_version: MAJOR_VERSION,
        minor_version: MINOR_VERSION,
        max_cpus: i32::try_from(MAX_CPUS).unwrap(),
        memory_size: vm::page::memory_size(),
    };
    assert!(INFO.set(info).is_ok(), "machine: information filled twice");
}
