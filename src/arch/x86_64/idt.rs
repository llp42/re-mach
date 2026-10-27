// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Interrupt descriptor tables, one per CPU.

use core::ptr;
use core::sync::atomic::{AtomicU64, Ordering};

use super::gdt::{KERNEL_CODE_INDEX, KERNEL_PRIVILEGE, selector};
use super::instructions::{self, PseudoDescriptor};
use super::trap;
use crate::smp::PerCpu;

/// A 64-bit IDT gate; atomic halves, so it can be set through `&self`.
#[repr(C)]
pub struct Gate {
    low: AtomicU64,
    high: AtomicU64,
}

impl Gate {
    /// Not present.
    pub const fn missing() -> Self {
        Self {
            low: AtomicU64::new(0),
            high: AtomicU64::new(0),
        }
    }

    /// Makes this an interrupt gate (type 0xe), present, ring 0, no IST.
    ///
    /// # Safety
    ///
    /// `handler` is an interrupt entry point, and this vector can't fire
    /// while the two halves are written.
    pub unsafe fn set(&self, handler: u64, selector: u16) {
        self.high.store(handler >> 32, Ordering::Relaxed);
        self.low.store(
            (handler & 0xffff)
                | u64::from(selector) << 16
                | 0x8e << 40
                | (handler & 0xffff_0000) << 32,
            Ordering::Relaxed,
        );
    }
}

/// An IDT.
#[repr(C)]
pub struct Idt([Gate; 256]);

impl Idt {
    /// Every gate not present.
    pub const fn new() -> Self {
        Self([const { Gate::missing() }; 256])
    }

    /// Loads this IDT.
    pub fn load(&'static self) {
        let descriptor = PseudoDescriptor {
            limit: u16::try_from(size_of_val(self) - 1).unwrap(),
            base: ptr::from_ref(self).cast(),
        };
        // SAFETY: `self` is static, and present gates only come from
        // `Gate::set`, whose callers vouch for the handler.
        unsafe { instructions::load_idt(&descriptor) };
    }
}

/// Points every vector of `per_cpu`'s IDT at its trap stub and loads it.
pub fn init(per_cpu: &'static PerCpu) {
    let idt = &per_cpu.md.idt;
    for (vector, gate) in (0..=u8::MAX).zip(&idt.0) {
        // SAFETY: the stub is this vector's entry point; the IDT isn't loaded yet.
        unsafe {
            gate.set(
                trap::stub(vector),
                selector(KERNEL_CODE_INDEX, KERNEL_PRIVILEGE),
            );
        }
    }
    idt.load();
}
