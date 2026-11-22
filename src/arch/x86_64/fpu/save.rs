// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::sync::OnceLock;

/// How a thread's floating-point state is saved; always in the standard
/// layout, so the state a thread reads back is laid out alike on every CPU.
#[derive(Clone, Copy)]
pub(super) enum Save {
    /// FXSAVE: x87 and SSE.
    Fxsave,
    /// XSAVE: every state XCR0 enables.
    Xsave,
    /// XSAVEOPT: as XSAVE, skipping the states unchanged since the last
    /// restore.
    XsaveOptimized,
}

impl Save {
    /// Its number in `i386_XFLOAT_STATE`, GNU Mach's `fp_save_kind`.
    pub(super) const fn number(self) -> u32 {
        match self {
            Self::Fxsave => 1,
            Self::Xsave => 2,
            Self::XsaveOptimized => 3,
        }
    }
}

/// The save instruction, once `init` picks it.
pub(super) static SAVE: OnceLock<Save> = OnceLock::new();

/// The states XCR0 enables, when the CPU has XSAVE.
pub(super) static FEATURES: OnceLock<u64> = OnceLock::new();

/// The bytes a thread's saved state takes.
pub(super) static STATE_SIZE: OnceLock<usize> = OnceLock::new();

/// The size of FXSAVE's area.
pub(super) const FXSAVE_SIZE: usize = 512;

/// FXSAVE's area in its 64-bit form, then the XSAVE header, as
/// `i386_xfp_save`: a whole saved state under FXSAVE, and the start of one
/// under XSAVE.
#[repr(C, align(64))]
pub(super) struct SaveArea {
    pub(super) control: u16,
    pub(super) status: u16,
    /// The abridged tag word: a bit set for each physical register not
    /// empty.
    pub(super) tag: u8,
    pub(super) reserved: u8,
    pub(super) opcode: u16,
    /// The last x87 instruction's address.
    pub(super) instruction_pointer: u64,
    /// Its operand's address.
    pub(super) data_pointer: u64,
    pub(super) mxcsr: u32,
    /// The MXCSR bits the CPU supports.
    pub(super) mxcsr_mask: u32,
    /// The x87 registers in stack order, 10 bytes of 16 each.
    pub(super) registers: [[u8; 16]; 8],
    pub(super) xmm: [[u8; 16]; 16],
    pub(super) padding: [u8; 96],
    /// `XSTATE_BV`: the states the area holds.
    pub(super) components: u64,
    /// `XCOMP_BV`: 0, the standard layout.
    pub(super) compaction: u64,
    pub(super) header_reserved: [u8; 48],
}

/// Where a saved state's area starts in its buffer, past its header; the
/// areas XSAVE writes are 64-byte aligned.
pub(super) const AREA_OFFSET: usize = 64;

/// The abridged tag word of a full one: a bit set for each register not
/// empty (3).
pub(super) fn abridged_tag(tag: u16) -> u8 {
    (0..8).fold(0, |abridged, physical: u16| {
        if tag >> (2 * physical) & 3 == 3 {
            abridged
        } else {
            abridged | 1 << physical
        }
    })
}

/// The full tag word of `area`'s x87 registers, from its abridged one and
/// their values: for each physical register, 3 empty, 2 special, 1 zero or
/// 0 valid.
pub(super) fn full_tag(area: &SaveArea) -> u16 {
    let top = area.status >> 11 & 7;
    (0..8).fold(0, |tag, physical: u16| {
        let class = if area.tag & 1 << physical == 0 {
            3
        } else {
            let register = &area.registers[usize::from(physical.wrapping_sub(top) & 7)];
            let significand = u64::from_le_bytes(*register.first_chunk().unwrap());
            let exponent = u16::from_le_bytes([register[8], register[9]]) & 0x7fff;
            match exponent {
                0 if significand == 0 => 1,
                // A normal number, its integer bit set.
                1..0x7fff if significand >> 63 == 1 => 0,
                _ => 2,
            }
        };
        tag | class << (2 * physical)
    })
}
