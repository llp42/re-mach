// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Interrupt descriptor tables, one per CPU.

use x86_64::VirtAddr;
use x86_64::instructions::segmentation::{CS, Segment};
use x86_64::instructions::tables::lidt;
use x86_64::structures::DescriptorTablePointer;
use x86_64::structures::gdt::SegmentSelector;

use super::trap;
use crate::smp::PerCpu;

/// A 64-bit IDT gate.
#[repr(C)]
pub struct Gate {
    low: u64,
    high: u64,
}

impl Gate {
    /// Not present.
    pub const fn missing() -> Self {
        Self { low: 0, high: 0 }
    }

    /// Interrupt gate (type 0xe), present, ring 0, no IST.
    pub fn new(handler: VirtAddr, selector: SegmentSelector) -> Self {
        let offset = handler.as_u64();
        Self {
            low: (offset & 0xffff)
                | u64::from(selector.0) << 16
                | 0x8e << 40
                | (offset & 0xffff_0000) << 32,
            high: offset >> 32,
        }
    }
}

/// Points every vector of `per_cpu`'s IDT at its trap stub and loads it.
pub fn init(per_cpu: &'static PerCpu) {
    let selector = CS::get_reg();
    // SAFETY: only this CPU touches its IDT, and it isn't loaded yet.
    let idt = unsafe { &mut *per_cpu.md.idt.get() };
    for vector in 0..=u8::MAX {
        idt[usize::from(vector)] = Gate::new(trap::stub(vector), selector);
    }
    let pointer = DescriptorTablePointer {
        limit: u16::try_from(size_of_val(idt) - 1).unwrap(),
        base: VirtAddr::from_ptr(idt),
    };
    // SAFETY: every gate points to a trap stub; `idt` lives in a static.
    unsafe { lidt(&pointer) };
}
