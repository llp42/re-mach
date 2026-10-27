// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Trap entry stubs and the trap report.

use core::arch::naked_asm;
use core::fmt::Write;

use super::halt;
use super::instructions;
use super::serial::Serial;

/// What the stubs and the CPU pushed, lowest address first.
#[repr(C)]
pub struct TrapFrame {
    pub vector: u64,
    /// 0 for vectors where the CPU pushes none.
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
}

/// Names of vectors 0-31.
const NAMES: [&str; 32] = [
    "#DE",
    "#DB",
    "NMI",
    "#BP",
    "#OF",
    "#BR",
    "#UD",
    "#NM",
    "#DF",
    "coprocessor overrun",
    "#TS",
    "#NP",
    "#SS",
    "#GP",
    "#PF",
    "reserved",
    "#MF",
    "#AC",
    "#MC",
    "#XM",
    "#VE",
    "#CP",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "reserved",
    "#HV",
    "#VC",
    "#SX",
    "reserved",
];

/// 256 stubs, 16 bytes apart from the first 16-byte boundary: each pushes its
/// vector (and a 0 error code if the CPU pushes none), then calls `trap`.
#[unsafe(naked)]
unsafe extern "sysv64" fn stubs() {
    naked_asm!(
        ".set trap_vector, 0",
        ".rept 256",
        ".balign 16",
        ".if trap_vector == 8 || (trap_vector >= 10 && trap_vector <= 14) || trap_vector == 17 || trap_vector == 21 || trap_vector == 29 || trap_vector == 30",
        ".else",
        "pushq $0",
        ".endif",
        "pushq $trap_vector",
        "jmp 2f",
        ".set trap_vector, trap_vector + 1",
        ".endr",
        "2:",
        "mov %rsp, %rdi",
        "and $-16, %rsp",
        "cld",
        "call {trap}",
        trap = sym trap,
        options(att_syntax),
    );
}

/// Entry stub for `vector`.
pub fn stub(vector: u8) -> u64 {
    let base = u64::try_from((stubs as *const ()).expose_provenance()).unwrap();
    base.next_multiple_of(16) + u64::from(vector) * 16
}

/// Reports a trap on COM1 and halts.
extern "sysv64" fn trap(frame: &TrapFrame) -> ! {
    let name = usize::try_from(frame.vector)
        .ok()
        .and_then(|vector| NAMES.get(vector))
        .copied()
        .unwrap_or("interrupt");
    let _ = write!(
        Serial,
        "trap {} {} error {:#x} rip {:#x} cs {:#x} rflags {:#x} rsp {:#x}",
        frame.vector, name, frame.error_code, frame.rip, frame.cs, frame.rflags, frame.rsp
    );
    if frame.vector == 14 {
        let _ = write!(Serial, " cr2 {:#x}", instructions::read_cr2());
    }
    let _ = write!(Serial, "\r\n");
    halt()
}
