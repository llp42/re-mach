// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Threads' user state, as `thread_get_state` gives it and
//! `thread_set_state` takes it.

/// A thread state's flavor, as Mach numbers it on x86; only those written
/// so far.
pub(super) enum StateFlavor {
    /// `i386_THREAD_STATE`: the user registers; a set forces the user
    /// segments.
    Registers,
    /// `i386_REGS_SEGS_STATE`: the user registers and segments.
    RegistersAndSegments,
    /// `i386_DEBUG_STATE`: the debug registers.
    Debug,
    /// `i386_FSGS_BASE_STATE`: the FS and GS bases.
    FsGsBase,
}

impl TryFrom<i32> for StateFlavor {
    type Error = i32;

    /// The flavor of Mach's number `number`, if it is one written here.
    fn try_from(number: i32) -> Result<Self, i32> {
        match number {
            1 => Ok(Self::Registers),
            5 => Ok(Self::RegistersAndSegments),
            6 => Ok(Self::Debug),
            7 => Ok(Self::FsGsBase),
            _ => Err(number),
        }
    }
}

/// `i386_thread_state`, as Mach lays it out on `x86_64`: a thread's user
/// registers and segments; its padding is fields, zero as given, ignored as
/// taken.
#[repr(C)]
pub(super) struct UserRegisters {
    pub(super) r8: u64,
    pub(super) r9: u64,
    pub(super) r10: u64,
    pub(super) r11: u64,
    pub(super) r12: u64,
    pub(super) r13: u64,
    pub(super) r14: u64,
    pub(super) r15: u64,
    pub(super) rdi: u64,
    pub(super) rsi: u64,
    pub(super) rbp: u64,
    /// Unused: 0.
    pub(super) rsp: u64,
    pub(super) rbx: u64,
    pub(super) rdx: u64,
    pub(super) rcx: u64,
    pub(super) rax: u64,
    pub(super) rip: u64,
    pub(super) cs: u32,
    pub(super) cs_padding: u32,
    /// The flags.
    pub(super) rflags: u64,
    /// The user stack pointer.
    pub(super) user_rsp: u64,
    pub(super) ss: u32,
    pub(super) ss_padding: u32,
}

impl UserRegisters {
    /// Its size in words, `i386_THREAD_STATE_COUNT`.
    pub(super) const COUNT: usize = size_of::<Self>() / size_of::<u32>();
}

/// `i386_fsgs_base_state`: a thread's user FS and GS bases.
#[repr(C)]
pub(super) struct FsGsBase {
    pub(super) fs_base: u64,
    pub(super) gs_base: u64,
}

impl FsGsBase {
    /// Its size in words, `i386_FSGS_BASE_STATE_COUNT`.
    pub(super) const COUNT: usize = size_of::<Self>() / size_of::<u32>();
}
