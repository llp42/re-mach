// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The interrupt descriptor table, one shared by every CPU.

use core::cell::RefMut;
use core::mem;

use x86_64::structures::idt::{Entry, HandlerFunc, InterruptDescriptorTable};
use x86_64::{PrivilegeLevel, VirtAddr};

use super::trap;
use crate::sync::{BootCell, OnceLock};

/// The table's storage, which the boot CPU fills before publishing it.
static TABLE: BootCell<InterruptDescriptorTable> = BootCell::new(InterruptDescriptorTable::new());

/// The table once published: read-only, and loaded by every CPU.
static IDT: OnceLock<&'static InterruptDescriptorTable> = OnceLock::new();

/// Fills the IDT with the exception entries; `interrupt::init` fills the
/// interrupt vectors.
///
/// # Safety
///
/// Runs once, on the boot CPU, after `gdt::init`.
pub unsafe fn init() {
    let mut idt = TABLE.borrow_mut();
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
}

/// The IDT's interrupt vectors, 0x20 to 0xff, until it is published.
pub fn interrupts() -> RefMut<'static, [Entry<HandlerFunc>]> {
    RefMut::map(TABLE.borrow_mut(), |idt| idt.slice_mut(32..))
}

/// Publishes the filled IDT, read-only from then on.
pub fn publish() {
    let table = TABLE.borrow();
    let published: *const InterruptDescriptorTable = &raw const *table;
    // The borrow never ends, so no later `borrow_mut` can fill the table.
    mem::forget(table);
    // SAFETY: the table is a static, which the forgotten borrow keeps
    // shared for ever.
    let published = unsafe { &*published };
    assert!(IDT.set(published).is_ok(), "idt: published twice");
}

/// Loads the published IDT on the current CPU.
pub fn load() {
    IDT.get().expect("idt: not published").load();
}
