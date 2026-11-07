// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Machine slots: what the host knows of each processor.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use crate::arch::{self, Machine as _};
use crate::config::MAX_CPUS;
use crate::host::Host;
use crate::processor;
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
    /// Whether the processor is up.
    pub running: AtomicBool,
    /// Its ticks, by `CpuState`.
    pub cpu_ticks: [AtomicU32; CPU_STATES],
}

/// How many states a CPU's ticks are counted in.
const CPU_STATES: usize = 3;

/// What a CPU spent a tick on, as Mach numbers it; only the states used
/// here.
#[derive(Clone, Copy)]
pub enum CpuState {
    /// User code.
    User = 0,
    /// The kernel.
    System = 1,
    /// Nothing: the CPU idles.
    Idle = 2,
}

impl CpuState {
    /// Its number, the index of its ticks.
    const fn index(self) -> usize {
        match self {
            Self::User => 0,
            Self::System => 1,
            Self::Idle => 2,
        }
    }
}

impl Slot {
    /// A slot holding a processor of `ty` and `subtype`, not up yet.
    pub const fn new(ty: CpuType, subtype: CpuSubtype) -> Self {
        Self {
            ty,
            subtype,
            running: AtomicBool::new(false),
            cpu_ticks: [const { AtomicU32::new(0) }; CPU_STATES],
        }
    }
}

/// One per processor, set once it is found.
static SLOTS: [OnceLock<Slot>; MAX_CPUS] = [const { OnceLock::new() }; MAX_CPUS];

/// Whether slot `cpu` holds a processor.
pub fn has_processor(cpu: usize) -> bool {
    SLOTS[cpu].get().is_some()
}

/// How many slots hold a processor.
pub fn processor_count() -> usize {
    SLOTS.iter().filter(|slot| slot.get().is_some()).count()
}

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

/// How many processors are up.
static AVAILABLE_CPUS: AtomicU32 = AtomicU32::new(0);

/// Brings processor `cpu` up, on its CPU: its slot says it runs, it counts
/// as available, and its processor joins its set.
///
/// # Panics
///
/// If slot `cpu` holds no processor.
pub fn cpu_up(cpu: usize) {
    let slot = SLOTS[cpu].get().expect("cpu_up: no processor in the slot");
    slot.running.store(true, Ordering::Release);
    AVAILABLE_CPUS.fetch_add(1, Ordering::Relaxed);
    processor::up(cpu);
}

/// Counts a tick processor `cpu` spent in `state`, on its CPU.
pub fn count_tick(cpu: usize, state: CpuState) {
    if let Some(slot) = SLOTS[cpu].get() {
        let ticks = &slot.cpu_ticks[state.index()];
        ticks.store(ticks.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
    }
}

/// Reboots the machine, as `host_reboot` does with `host`, the host a
/// privileged port stands for, and no options.
///
/// # Panics
///
/// With no host, or with options: not yet.
pub fn host_reboot(host: Option<&Host>, options: i32) -> ! {
    assert!(host.is_some(), "host_reboot: KERN_INVALID_HOST, not yet");
    assert!(options == 0, "host_reboot: options {options:#x}, not yet");
    arch::Current::halt_all_cpus()
}
