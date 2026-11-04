// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Local APIC and IO APICs; `enable` chooses the local APIC's mode.

use x86_64::registers::model_specific::{ApicBase, ApicBaseFlags};

use super::counter;
use super::mmio::Registers;
use super::per_cpu::PerCpu;
use crate::collections::ArrayVec;
use crate::config::{HZ, MAX_CPUS};
use crate::println;
use crate::smp::{self, MachinePerCpu as _};
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
    pub gsi_base: u32,
    /// How many inputs it has.
    pub inputs: u32,
    /// Its registers.
    pub registers: Registers,
}

/// What an IO APIC input's polarity is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    /// Asserted high.
    High,
    /// Asserted low.
    Low,
}

/// How an IO APIC input is triggered.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// By an edge.
    Edge,
    /// By a level.
    Level,
}

/// An ISA interrupt wired to another global system interrupt, or with
/// other modes than the bus's.
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

/// The first vector of devices' interrupts: one per global system
/// interrupt from here.
pub const DEVICE_VECTOR_BASE: u32 = 0x30;

/// The local APIC's spurious vector.
pub const SPURIOUS_VECTOR: u32 = 0xff;

/// Local APIC registers, by byte offset.
mod local {
    pub const TASK_PRIORITY: usize = 0x80;
    pub const END_OF_INTERRUPT: usize = 0xb0;
    pub const SPURIOUS: usize = 0xf0;
    pub const ERROR_STATUS: usize = 0x280;
    pub const TIMER: usize = 0x320;
    pub const PERFORMANCE: usize = 0x340;
    pub const LINT0: usize = 0x350;
    pub const LINT1: usize = 0x360;
    pub const INITIAL_COUNT: usize = 0x380;
    pub const CURRENT_COUNT: usize = 0x390;
    pub const DIVIDE: usize = 0x3e0;
}

/// The local APIC timer's vector: above every device's, below the IPIs'.
pub const TIMER_VECTOR: usize = 0xf0;

/// The timer's divide configuration: by 2.
const DIVIDE_BY_2: u32 = 0;

/// The timer's periodic mode.
const PERIODIC: u32 = 1 << 17;

/// How long the timer is measured against the time counter: 10 ticks.
const CALIBRATION_TICKS: u64 = 10;

/// The timer's count for one tick, once calibrated.
static TICK_COUNT: OnceLock<u32> = OnceLock::new();

/// A local vector table entry's mask bit.
const MASKED: u32 = 1 << 16;

/// The spurious register's bit that turns the local APIC on.
const SOFTWARE_ENABLE: u32 = 1 << 8;

/// A local vector table entry's NMI delivery mode.
const NMI: u32 = 4 << 8;

/// An IO APIC's select and window registers, by byte offset.
const SELECT: usize = 0;
const WINDOW: usize = 0x10;

/// The IO APIC's version register, which holds its last input's number.
pub const IO_APIC_VERSION: u32 = 1;

/// The IO APIC's first redirection register: two for each input, low then
/// high.
const IO_APIC_ROUTES: u32 = 0x10;

impl IoApic {
    /// Reads its register `index`.
    pub fn read(&self, index: u32) -> u32 {
        self.registers.write(SELECT, index);
        self.registers.read(WINDOW)
    }

    /// Writes `value` to its register `index`.
    fn write(&self, index: u32, value: u32) {
        self.registers.write(SELECT, index);
        self.registers.write(WINDOW, value);
    }

    /// Routes `input`, masked: to `vector` on the CPU of local APIC ID
    /// `destination`, fixed delivery, with its polarity and trigger.
    fn route(
        &self,
        input: u32,
        vector: u32,
        destination: u8,
        polarity: Polarity,
        trigger: Trigger,
    ) {
        let low = vector
            | MASKED
            | if polarity == Polarity::Low {
                1 << 13
            } else {
                0
            }
            | if trigger == Trigger::Level {
                1 << 15
            } else {
                0
            };
        self.write(IO_APIC_ROUTES + input * 2 + 1, u32::from(destination) << 24);
        self.write(IO_APIC_ROUTES + input * 2, low);
    }
}

/// Programs every IO APIC input masked, then sets the boot CPU's local APIC
/// up and turns it on, on the boot CPU: GSI `n` goes to vector
/// `DEVICE_VECTOR_BASE + n` on the boot CPU, ISA interrupts edge-triggered and
/// active-high, the others level-triggered and active-low, as ACPI defines
/// them, unless an override says otherwise.
///
/// # Panics
///
/// If the ACPI step found no APIC.
pub fn configure_io_apics() {
    let local = LOCAL_APIC.get().expect("ioapic_configure: no local APIC");
    let io_apics = IO_APICS.get().expect("ioapic_configure: no IO APIC");
    let overrides = OVERRIDES.get().expect("ioapic_configure: no overrides");
    // Off until set up.
    local.write(local::SPURIOUS, SPURIOUS_VECTOR);
    let destination = PerCpu::current().apic_id();
    for (index, io_apic) in io_apics.iter().enumerate() {
        println!("IOAPIC version {:#x}", io_apic.read(IO_APIC_VERSION) & 0xff);
        for input in 0..io_apic.inputs {
            let gsi = io_apic.gsi_base + input;
            let (mut polarity, mut trigger) = if gsi < 16 {
                (Polarity::High, Trigger::Edge)
            } else {
                (Polarity::Low, Trigger::Level)
            };
            if let Some(found) = overrides.iter().find(|found| found.gsi == gsi) {
                // An ISA interrupt: edge-triggered by default, and active-low
                // when level-triggered.
                trigger = found.trigger.unwrap_or(Trigger::Edge);
                polarity = found.polarity.unwrap_or(if trigger == Trigger::Level {
                    Polarity::Low
                } else {
                    Polarity::High
                });
                println!(
                    "IRQ override: pin={} gsi={gsi} trigger={} polarity={}",
                    found.irq,
                    if trigger == Trigger::Level {
                        "LEVEL"
                    } else {
                        "EDGE"
                    },
                    if polarity == Polarity::Low {
                        "LOW"
                    } else {
                        "HIGH"
                    }
                );
            }
            // GSIs past the vectors devices may use stay masked, vector 0.
            let vector = Some(DEVICE_VECTOR_BASE + gsi)
                .filter(|&vector| vector < 0xf0)
                .unwrap_or(0);
            io_apic.route(input, vector, destination, polarity, trigger);
        }
        println!(
            "IOAPIC {index} configured with GSI {}-{}",
            io_apic.gsi_base,
            io_apic.gsi_base + io_apic.inputs - 1
        );
    }
    setup_local(local);
}

/// Sets the current CPU's local APIC up and turns it on: LINT0, the timer
/// and the performance counter masked, LINT1 delivering NMIs, every
/// priority accepted.
fn setup_local(local: &Registers) {
    for register in [local::LINT0, local::TIMER, local::PERFORMANCE] {
        local.write(register, local.read(register) | MASKED);
    }
    local.write(local::LINT1, NMI);
    local.write(local::TASK_PRIORITY, 0);
    local.write(local::SPURIOUS, SPURIOUS_VECTOR | SOFTWARE_ENABLE);
    local.write(local::ERROR_STATUS, 0);
}

/// Calibrates the current CPU's local APIC timer against the time counter
/// over `CALIBRATION_TICKS` ticks, then starts it ticking `HZ` times a
/// second.
///
/// # Panics
///
/// If the ACPI step found no local APIC, or the timer did not count.
pub fn start_timer() {
    let local = LOCAL_APIC.get().expect("startrtclock: no local APIC");
    let count = if let Some(&count) = TICK_COUNT.get() {
        count
    } else {
        let count = calibrate(local);
        assert!(
            TICK_COUNT.set(count).is_ok(),
            "startrtclock: calibrated twice"
        );
        count
    };
    local.write(local::DIVIDE, DIVIDE_BY_2);
    local.write(
        local::TIMER,
        PERIODIC | u32::try_from(TIMER_VECTOR).unwrap(),
    );
    local.write(local::INITIAL_COUNT, count);
    println!("LAPIC timer configured on cpu{}", smp::current().id());
}

/// The timer's count for one tick, measured one-shot and masked.
fn calibrate(local: &Registers) -> u32 {
    local.write(local::DIVIDE, DIVIDE_BY_2);
    local.write(local::TIMER, MASKED | u32::try_from(TIMER_VECTOR).unwrap());
    let span = CALIBRATION_TICKS * 1_000_000_000 / u64::from(HZ);
    let start = counter::nanoseconds();
    local.write(local::INITIAL_COUNT, u32::MAX);
    while counter::nanoseconds() - start < span {}
    let counted = u32::MAX - local.read(local::CURRENT_COUNT);
    local.write(local::INITIAL_COUNT, 0);
    assert!(
        counted != 0,
        "startrtclock: the local APIC timer did not count"
    );
    println!("timer calibration... done");
    counted / u32::try_from(CALIBRATION_TICKS).unwrap()
}

/// Ends the interrupt the current CPU's local APIC is delivering.
pub fn end_of_interrupt() {
    if let Some(local) = LOCAL_APIC.get() {
        local.write(local::END_OF_INTERRUPT, 0);
    }
}
