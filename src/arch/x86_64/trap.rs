// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Traps: their entry points and the kernel's handler.

use core::arch::global_asm;
use core::fmt::{self, Write};
use core::mem::offset_of;
use core::ptr;
use core::sync::atomic::Ordering;

use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::registers::control::Cr2;
use x86_64::registers::rflags::RFlags;
use x86_64::structures::idt::{ExceptionVector, InterruptStackFrameValue, PageFaultErrorCode};

use super::memory_layout::{MAX_USER_ADDRESS, PAGE_SIZE};
use super::per_cpu::KERNEL_STACK;
use super::serial::Serial;
use super::{MachineState, fpu, gdt};
use crate::smp;
use crate::thread::MachineThread as _;
use crate::vm::kernel::KERNEL_MAP;
use crate::vm::{self, Protection};

// Each entry pushes a zero error code, unless the CPU pushed one, and its
// vector; the common path saves the registers as a `TrapFrame`. A trap from
// user mode, saved in the thread's machine state, swaps the GS base and
// calls `user_trap` on the kernel stack; one from kernel mode calls
// `kernel_trap` and returns.
global_asm!(
    ".pushsection .text, \"ax\"",
    ".macro entry name, vector",
    ".globl \\name",
    "\\name:",
    "pushq $0",
    "pushq $\\vector",
    "jmp .Ltrap_common",
    ".endm",
    ".macro entry_with_error_code name, vector",
    ".globl \\name",
    "\\name:",
    "pushq $\\vector",
    "jmp .Ltrap_common",
    ".endm",
    "entry divide_error_entry, 0",
    "entry debug_entry, 1",
    "entry non_maskable_interrupt_entry, 2",
    "entry breakpoint_entry, 3",
    "entry invalid_opcode_entry, 6",
    "entry device_not_available_entry, 7",
    "entry_with_error_code double_fault_entry, 8",
    "entry_with_error_code invalid_tss_entry, 10",
    "entry_with_error_code segment_not_present_entry, 11",
    "entry_with_error_code stack_segment_fault_entry, 12",
    "entry_with_error_code general_protection_fault_entry, 13",
    "entry_with_error_code page_fault_entry, 14",
    "entry x87_floating_point_entry, 16",
    "entry_with_error_code alignment_check_entry, 17",
    "entry machine_check_entry, 18",
    "entry simd_floating_point_entry, 19",
    "entry virtualization_entry, 20",
    "entry_with_error_code cp_protection_exception_entry, 21",
    "entry hv_injection_exception_entry, 28",
    "entry_with_error_code vmm_communication_exception_entry, 29",
    "entry_with_error_code security_exception_entry, 30",
    ".Ltrap_common:",
    "pushq %rax",
    "pushq %rbx",
    "pushq %rcx",
    "pushq %rdx",
    "pushq %rsi",
    "pushq %rdi",
    "pushq %rbp",
    "pushq %r8",
    "pushq %r9",
    "pushq %r10",
    "pushq %r11",
    "pushq %r12",
    "pushq %r13",
    "pushq %r14",
    "pushq %r15",
    "testb $3, {code_segment}(%rsp)",
    "jz .Ltrap_from_kernel",
    "swapgs",
    "cld",
    "movq %rsp, %rdi",
    "movq %gs:{kernel_stack}, %rsp",
    "call {user_trap}",
    "ud2",
    ".Ltrap_from_kernel:",
    "cld",
    "movq %rsp, %rdi",
    "call {kernel_trap}",
    "popq %r15",
    "popq %r14",
    "popq %r13",
    "popq %r12",
    "popq %r11",
    "popq %r10",
    "popq %r9",
    "popq %r8",
    "popq %rbp",
    "popq %rdi",
    "popq %rsi",
    "popq %rdx",
    "popq %rcx",
    "popq %rbx",
    "popq %rax",
    "addq $16, %rsp",
    "iretq",
    ".popsection",
    code_segment = const offset_of!(TrapFrame, interrupted)
        + offset_of!(InterruptStackFrameValue, code_segment),
    kernel_stack = const KERNEL_STACK,
    user_trap = sym user_trap,
    kernel_trap = sym kernel_trap,
    options(att_syntax),
);

// The entries, named after the IDT fields they fill.
unsafe extern "C" {
    pub safe static divide_error_entry: u8;
    pub safe static debug_entry: u8;
    pub safe static non_maskable_interrupt_entry: u8;
    pub safe static breakpoint_entry: u8;
    pub safe static invalid_opcode_entry: u8;
    pub safe static device_not_available_entry: u8;
    pub safe static double_fault_entry: u8;
    pub safe static invalid_tss_entry: u8;
    pub safe static segment_not_present_entry: u8;
    pub safe static stack_segment_fault_entry: u8;
    pub safe static general_protection_fault_entry: u8;
    pub safe static page_fault_entry: u8;
    pub safe static x87_floating_point_entry: u8;
    pub safe static alignment_check_entry: u8;
    pub safe static machine_check_entry: u8;
    pub safe static simd_floating_point_entry: u8;
    pub safe static virtualization_entry: u8;
    pub safe static cp_protection_exception_entry: u8;
    pub safe static hv_injection_exception_entry: u8;
    pub safe static vmm_communication_exception_entry: u8;
    pub safe static security_exception_entry: u8;
}

/// Registers saved on a trap, from the stack top down.
#[repr(C)]
pub struct TrapFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    pub(super) r10: u64,
    pub(super) r9: u64,
    pub(super) r8: u64,
    rbp: u64,
    pub(super) rdi: u64,
    pub(super) rsi: u64,
    pub(super) rdx: u64,
    rcx: u64,
    rbx: u64,
    pub(super) rax: u64,
    pub vector: usize,
    error_code: u64,
    /// Pushed by the CPU.
    pub interrupted: InterruptStackFrameValue,
}

impl TrapFrame {
    /// A new thread's: all zero but user mode's segments and interrupts
    /// on, so the thread starts in user mode.
    pub fn user() -> Self {
        Self {
            r15: 0,
            r14: 0,
            r13: 0,
            r12: 0,
            r11: 0,
            r10: 0,
            r9: 0,
            r8: 0,
            rbp: 0,
            rdi: 0,
            rsi: 0,
            rdx: 0,
            rcx: 0,
            rbx: 0,
            rax: 0,
            vector: 0,
            error_code: 0,
            interrupted: InterruptStackFrameValue::new(
                VirtAddr::zero(),
                gdt::USER_CODE,
                RFlags::INTERRUPT_FLAG,
                VirtAddr::zero(),
                gdt::USER_DATA,
            ),
        }
    }
}

impl fmt::Display for TrapFrame {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let interrupted = &self.interrupted;
        write!(
            f,
            "rax {:#018x} rbx {:#018x} rcx {:#018x} rdx {:#018x}\r\n\
             rsi {:#018x} rdi {:#018x} rbp {:#018x} rsp {:#018x}\r\n\
             r8  {:#018x} r9  {:#018x} r10 {:#018x} r11 {:#018x}\r\n\
             r12 {:#018x} r13 {:#018x} r14 {:#018x} r15 {:#018x}\r\n\
             rflags {:#018x} cs {:#06x} ss {:#06x}\r\n",
            self.rax,
            self.rbx,
            self.rcx,
            self.rdx,
            self.rsi,
            self.rdi,
            self.rbp,
            interrupted.stack_pointer.as_u64(),
            self.r8,
            self.r9,
            self.r10,
            self.r11,
            self.r12,
            self.r13,
            self.r14,
            self.r15,
            interrupted.cpu_flags.bits(),
            interrupted.code_segment.0,
            interrupted.stack_segment.0,
        )
    }
}

/// Takes a trap from user mode, then returns the thread to user mode: a page
/// fault is resolved in its task's map, and a first use of the FPU gives the
/// thread the FPU.
///
/// # Panics
///
/// On any other trap, or a page fault that does not resolve or is in kernel
/// space: they raise exceptions, not yet.
extern "sysv64" fn user_trap(frame: &TrapFrame) -> ! {
    let cr2 = Cr2::read_raw();
    // The interrupt gate turned interrupts off; user mode had them on.
    if frame.interrupted.cpu_flags.contains(RFlags::INTERRUPT_FLAG) {
        interrupts::enable();
    }
    let vector = u8::try_from(frame.vector)
        .ok()
        .and_then(|vector| ExceptionVector::try_from(vector).ok());
    match vector {
        Some(ExceptionVector::Page) => {
            let address = usize::try_from(cr2).unwrap();
            assert!(
                address < MAX_USER_ADDRESS,
                "i386_exception: a page fault in kernel space, not yet"
            );
            let thread = smp::current()
                .active_thread()
                .expect("user_trap: no current thread");
            let access = if PageFaultErrorCode::from_bits_truncate(frame.error_code)
                .contains(PageFaultErrorCode::CAUSED_BY_WRITE)
            {
                Protection::READ | Protection::WRITE
            } else {
                Protection::READ
            };
            vm::fault::fault(&thread.task().map, address & !(PAGE_SIZE - 1), access)
                .unwrap_or_else(|error| {
                    panic!("user_page_fault_continue: i386_exception {error:?}, not yet")
                });
        }
        Some(ExceptionVector::DeviceNotAvailable) => fpu::no_extension_fault(),
        _ => {
            let _ = write!(
                Serial,
                "user trap {}, code {:#x}, cr2 {cr2:#x}\r\n{frame}",
                frame.vector, frame.error_code,
            );
            panic!("i386_exception: not yet")
        }
    }
    MachineState::return_to_user()
}

/// Takes a trap from kernel mode: a page fault on a user address is
/// resolved in the current task's map, or resumes the copy that took it at
/// its recovery point; any other trap is reported and panics.
///
/// # Panics
///
/// On any other trap; on a page fault in kernel space or at address 0, not
/// yet; on one on a user address for an instruction fetch, in a kernel
/// thread, or that neither resolves nor recovers.
extern "sysv64" fn kernel_trap(frame: &mut TrapFrame) {
    let cr2 = Cr2::read_raw();
    let address = usize::try_from(cr2).unwrap();
    let page_fault = u8::try_from(frame.vector)
        .ok()
        .and_then(|vector| ExceptionVector::try_from(vector).ok())
        == Some(ExceptionVector::Page);
    // GNU Mach takes address 0 in the kernel map.
    let user = address != 0 && address < MAX_USER_ADDRESS;
    // The kernel runs no code at user addresses: such a fetch is fatal.
    let fetch = page_fault
        && PageFaultErrorCode::from_bits_truncate(frame.error_code)
            .contains(PageFaultErrorCode::INSTRUCTION_FETCH);
    if page_fault && user && !fetch {
        // The interrupt gate turned interrupts off; the fault had them as
        // its frame says.
        if frame.interrupted.cpu_flags.contains(RFlags::INTERRUPT_FLAG) {
            interrupts::enable();
        }
        let thread = smp::current().active_thread().filter(|thread| {
            !KERNEL_MAP
                .get()
                .is_some_and(|kernel| ptr::eq(kernel, &raw const *thread.task().map))
        });
        let Some(thread) = thread else {
            let _ = write!(Serial, "kernel page fault at {address:#x}:\r\n{frame}");
            panic!("kernel thread accessed user space!");
        };
        let access = if PageFaultErrorCode::from_bits_truncate(frame.error_code)
            .contains(PageFaultErrorCode::CAUSED_BY_WRITE)
        {
            Protection::READ | Protection::WRITE
        } else {
            Protection::READ
        };
        if vm::fault::fault(&thread.task().map, address & !(PAGE_SIZE - 1), access).is_ok() {
            return;
        }
        // A copy's recovery point, for a fault at its stack pointer.
        let recovery = thread.machine_state().onfault.load(Ordering::Relaxed);
        let stack = thread.machine_state().onfault_stack.load(Ordering::Relaxed);
        if recovery != 0
            && u64::try_from(stack).unwrap() == frame.interrupted.stack_pointer.as_u64()
        {
            frame.interrupted.instruction_pointer = VirtAddr::new(u64::try_from(recovery).unwrap());
            return;
        }
    }
    let _ = write!(
        Serial,
        "Kernel trap {}, rip {:#x}, code {:#x}, cr2 {cr2:#x}\r\n{frame}",
        frame.vector,
        frame.interrupted.instruction_pointer.as_u64(),
        frame.error_code,
    );
    assert!(
        !page_fault || user,
        "kernel_trap: a page fault in the kernel map, not yet"
    );
    assert!(
        !fetch,
        "kernel_trap: an instruction fetch at a user address"
    );
    panic!("trap")
}
