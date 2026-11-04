// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Local APIC and IO APICs; `enable` chooses the local APIC's mode.

use x86_64::registers::model_specific::{ApicBase, ApicBaseFlags};

use super::mmio::Registers;
use crate::collections::ArrayVec;
use crate::config::MAX_CPUS;
use crate::sync::OnceLock;

/// Turns on the boot CPU's local APIC in xAPIC mode.
pub fn enable() {
    let (base, mut flags) = ApicBase::read();
    if flags.contains(ApicBaseFlags::X2APIC_ENABLE) {
        // TODO: support x2APIC; keep it here instead of leaving it.
        flags.remove(ApicBaseFlags::LAPIC_ENABLE | ApicBaseFlags::X2APIC_ENABLE);
        // SAFETY: same base; x2APIC to disabled, the only way back to xAPIC.
        unsafe { ApicBase::write(base, flags) };
    }
    flags.insert(ApicBaseFlags::LAPIC_ENABLE | ApicBaseFlags::BSP);
    // SAFETY: same base; disabled or xAPIC to xAPIC.
    unsafe { ApicBase::write(base, flags) };
}

/// Most IO APICs kept.
pub const MAX_IO_APICS: usize = 16;

/// Most interrupt overrides kept.
pub const MAX_OVERRIDES: usize = 24;

/// An IO APIC: where its interrupts start, how many it has, its registers.
pub struct IoApic {
    /// Its APIC ID.
    pub id: u8,
    /// The global system interrupt its first input is.
    #[expect(dead_code, reason = "ioapic_configure reads it")]
    pub gsi_base: u32,
    /// How many inputs it has.
    #[expect(dead_code, reason = "ioapic_configure reads it")]
    pub inputs: u32,
    /// Its registers.
    pub registers: Registers,
}

/// What an IO APIC input's polarity is.
pub enum Polarity {
    /// Asserted high.
    High,
    /// Asserted low.
    Low,
}

/// How an IO APIC input is triggered.
pub enum Trigger {
    /// By an edge.
    Edge,
    /// By a level.
    Level,
}

/// An ISA interrupt wired to another global system interrupt, or with
/// other modes than the bus's.
#[expect(dead_code, reason = "ioapic_configure reads them")]
pub struct Override {
    /// The ISA interrupt.
    pub irq: u8,
    /// The global system interrupt it arrives on.
    pub gsi: u32,
    /// Its polarity, if not the bus's.
    pub polarity: Option<Polarity>,
    /// Its trigger, if not the bus's.
    pub trigger: Option<Trigger>,
}

/// Each CPU's local APIC ID, by CPU number: the boot CPU's first.
pub static APIC_IDS: OnceLock<ArrayVec<u8, MAX_CPUS>> = OnceLock::new();

/// The local APIC's registers, at the address every CPU sees its own.
pub static LOCAL_APIC: OnceLock<Registers> = OnceLock::new();

/// The IO APICs, in the order the firmware lists them.
pub static IO_APICS: OnceLock<ArrayVec<IoApic, MAX_IO_APICS>> = OnceLock::new();

/// The interrupt overrides.
pub static OVERRIDES: OnceLock<ArrayVec<Override, MAX_OVERRIDES>> = OnceLock::new();
