// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The interrupt descriptor table.

use core::cell::UnsafeCell;

use x86_64::structures::idt::{Entry, HandlerFunc, InterruptDescriptorTable};
use x86_64::{PrivilegeLevel, VirtAddr};

use super::trap;

struct Table(UnsafeCell<InterruptDescriptorTable>);

// SAFETY: written on the boot CPU, by `init` and `interrupt::init`, with
// interrupts off.
unsafe impl Sync for Table {}

/// The boot CPU's.
static IDT: Table = Table(UnsafeCell::new(InterruptDescriptorTable::new()));

/// Fills the boot CPU's IDT with the exception entries and loads it;
/// `interrupt::init` fills the interrupt vectors.
///
/// # Safety
///
/// Runs once, on the boot CPU, after `gdt::init`.
pub unsafe fn init() {
    // SAFETY: guaranteed by the caller; the table is not loaded yet.
    let idt = unsafe { &mut *IDT.0.get() };
    // SAFETY: each address is its vector's entry, built for the frame the
    // CPU pushes there. IST 1 is the double fault's, which never returns,
    // and the NMI's; IST 2 the debug exception's.
    unsafe {
        idt.divide_error
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::divide_error_entry));
        idt.debug
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::debug_entry))
            .set_stack_index(1);
        idt.non_maskable_interrupt
            .set_handler_addr(VirtAddr::from_ptr(
                &raw const trap::non_maskable_interrupt_entry,
            ))
            .set_stack_index(0);
        idt.breakpoint
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::breakpoint_entry))
            .set_privilege_level(PrivilegeLevel::Ring3);
        idt.invalid_opcode
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::invalid_opcode_entry));
        idt.device_not_available
            .set_handler_addr(VirtAddr::from_ptr(
                &raw const trap::device_not_available_entry,
            ));
        idt.double_fault
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::double_fault_entry))
            .set_stack_index(0);
        idt.invalid_tss
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::invalid_tss_entry));
        idt.segment_not_present.set_handler_addr(VirtAddr::from_ptr(
            &raw const trap::segment_not_present_entry,
        ));
        idt.stack_segment_fault.set_handler_addr(VirtAddr::from_ptr(
            &raw const trap::stack_segment_fault_entry,
        ));
        idt.general_protection_fault
            .set_handler_addr(VirtAddr::from_ptr(
                &raw const trap::general_protection_fault_entry,
            ));
        idt.page_fault
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::page_fault_entry));
        idt.x87_floating_point.set_handler_addr(VirtAddr::from_ptr(
            &raw const trap::x87_floating_point_entry,
        ));
        idt.alignment_check
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::alignment_check_entry));
        idt.machine_check
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::machine_check_entry));
        idt.simd_floating_point.set_handler_addr(VirtAddr::from_ptr(
            &raw const trap::simd_floating_point_entry,
        ));
        idt.virtualization
            .set_handler_addr(VirtAddr::from_ptr(&raw const trap::virtualization_entry));
        idt.cp_protection_exception
            .set_handler_addr(VirtAddr::from_ptr(
                &raw const trap::cp_protection_exception_entry,
            ));
        idt.hv_injection_exception
            .set_handler_addr(VirtAddr::from_ptr(
                &raw const trap::hv_injection_exception_entry,
            ));
        idt.vmm_communication_exception
            .set_handler_addr(VirtAddr::from_ptr(
                &raw const trap::vmm_communication_exception_entry,
            ));
        idt.security_exception.set_handler_addr(VirtAddr::from_ptr(
            &raw const trap::security_exception_entry,
        ));
    }
    // SAFETY: no `&mut` is alive; `interrupt::init` takes its own later.
    unsafe { &*IDT.0.get() }.load();
}

/// The boot CPU's interrupt vectors, 0x20 to 0xff.
///
/// # Safety
///
/// On the boot CPU, with interrupts off and no other reference to the
/// table alive.
pub unsafe fn interrupts() -> &'static mut [Entry<HandlerFunc>] {
    // SAFETY: guaranteed by the caller.
    unsafe { &mut *IDT.0.get() }.slice_mut(32..)
}
