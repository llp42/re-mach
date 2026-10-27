// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The kernel's page tables, like dfly's `pmap_bootstrap` and
//! `create_pagetables`, and the kernel's move to `KERNEL_BASE`.

use core::arch::asm;
use core::arch::x86_64::__cpuid;
use core::ptr;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::PAGE_SIZE;
use super::boot_memory;
use super::cpu::{EFER_NXE, EXTENDED_EDX_NX};
use super::instructions::{self, IA32_EFER};
use super::page_table::{self, ENTRIES, EXECUTE_DISABLE, LARGE_PAGE, PRESENT, PageTable, WRITABLE};
use crate::pe::{self, Image};

/// Bytes a page directory entry maps (2 MiB) and a PDPT entry (1 GiB).
const LARGE_PAGE_SIZE: u64 = 1 << 21;
const GIGABYTE: u64 = 1 << 30;

/// The direct map's first PML4 slot (dfly's `DMPML4I`).
const DIRECT_MAP_SLOT: usize = 368;
/// The direct map of physical memory, PML4 slot 368 (dfly's
/// `DMAP_MIN_ADDRESS`).
pub const DIRECT_MAP_BASE: u64 = 0xffff_b800_0000_0000;
/// The direct map covers at least this, as dfly's.
const DIRECT_MAP_MIN_SIZE: u64 = 512 * GIGABYTE;
/// Where the kernel image runs: -2 GiB (dfly's `KERNBASE`).
pub const KERNEL_BASE: u64 = 0xffff_ffff_8000_0000;

/// The kernel's PML4, physical.
static KERNEL_PML4: AtomicU64 = AtomicU64::new(0);
/// PML4 slots the temporary identity map takes, from 0.
static IDENTITY_SLOTS: AtomicUsize = AtomicUsize::new(0);

/// Builds the kernel's page tables and switches to them, moves the kernel to
/// `KERNEL_BASE` and calls `next` there. `guards` are pages to leave
/// unmapped, below stacks.
///
/// # Safety
///
/// Runs once, on the boot CPU, identity-mapped, with `boot_memory` ready;
/// `next` takes over for good.
pub unsafe fn bootstrap(next: extern "sysv64" fn() -> !, guards: &[u64]) -> ! {
    let execute_disable = enable_no_execute();
    let pml4 = allocate_table();
    // SAFETY: a fresh table, at its physical address until the move.
    let pml4_table = unsafe { table(pml4) };

    // The direct map, in 2 MiB pages; the identity map shares its tables,
    // without XD, until `remove_identity`.
    let size = boot_memory::end()
        .next_multiple_of(GIGABYTE)
        .max(DIRECT_MAP_MIN_SIZE);
    let mut directory_pointers = 0;
    for gigabyte in 0..size / GIGABYTE {
        let slot = usize::try_from(gigabyte / 512).unwrap();
        if gigabyte % 512 == 0 {
            directory_pointers = allocate_table();
            pml4_table.set(
                DIRECT_MAP_SLOT + slot,
                directory_pointers,
                PRESENT | WRITABLE | execute_disable,
            );
            pml4_table.set(slot, directory_pointers, PRESENT | WRITABLE);
            IDENTITY_SLOTS.store(slot + 1, Ordering::Relaxed);
        }
        let directory = allocate_table();
        // SAFETY: fresh tables, at their physical addresses until the move.
        let directory_table = unsafe { table(directory) };
        for entry in 0..ENTRIES {
            let address = gigabyte * GIGABYTE + u64::try_from(entry).unwrap() * LARGE_PAGE_SIZE;
            directory_table.set(entry, address, PRESENT | WRITABLE | LARGE_PAGE);
        }
        // SAFETY: as above.
        unsafe { table(directory_pointers) }.set(
            usize::try_from(gigabyte % 512).unwrap(),
            directory,
            PRESENT | WRITABLE,
        );
    }

    map_image(pml4_table, execute_disable, guards);
    KERNEL_PML4.store(pml4, Ordering::Relaxed);
    // SAFETY: the new tables map this code, its stack and data where they are.
    unsafe { instructions::write_cr3(pml4) };

    let image = Image::this();
    let delta = KERNEL_BASE.wrapping_sub(image.address());
    // SAFETY: the image is mapped both here and at `KERNEL_BASE`, so its
    // pointers stay valid either way.
    unsafe { image.relocate(delta) };
    let target = u64::try_from((next as *const ()).expose_provenance())
        .unwrap()
        .wrapping_add(delta);
    // SAFETY: `target` is `next` at `KERNEL_BASE`, and RSP + `delta` this
    // stack there.
    unsafe {
        asm!(
            "add {delta}, %rsp",
            "and $-16, %rsp",
            "xor %ebp, %ebp",
            "call *{target}",
            delta = in(reg) delta,
            target = in(reg) target,
            options(noreturn, att_syntax),
        );
    }
}

/// Drops the temporary identity map, once nothing runs or points there.
pub fn remove_identity() {
    let pml4 = KERNEL_PML4.load(Ordering::Relaxed);
    // SAFETY: the direct map holds the PML4.
    let pml4_table = unsafe { table(DIRECT_MAP_BASE + pml4) };
    for slot in 0..IDENTITY_SLOTS.load(Ordering::Relaxed) {
        pml4_table.clear(slot);
    }
    // SAFETY: the PML4 still maps the kernel, its stack and data.
    unsafe { instructions::write_cr3(pml4) };
}

/// Maps the kernel image at `KERNEL_BASE` in 4 KiB pages: headers read-only,
/// each section as its characteristics say, `guards` left out.
fn map_image(pml4: &PageTable, execute_disable: u64, guards: &[u64]) {
    let image = Image::this();
    let base = image.address();
    let directory_pointers = allocate_table();
    pml4.set(
        page_table::index(KERNEL_BASE, 3),
        directory_pointers,
        PRESENT | WRITABLE,
    );
    let directory = allocate_table();
    // SAFETY: fresh tables, at their physical addresses until the move.
    unsafe { table(directory_pointers) }.set(
        page_table::index(KERNEL_BASE, 2),
        directory,
        PRESENT | WRITABLE,
    );
    // SAFETY: as above.
    let directory = unsafe { table(directory) };
    // One page table per 2 MiB of image, allocated when first used.
    let mut page_tables = [0; ENTRIES];
    let mut map = |offset: u64, flags: u64| {
        if guards.contains(&(base + offset)) {
            return;
        }
        let address = KERNEL_BASE + offset;
        let slot = page_table::index(address, 1);
        if page_tables[slot] == 0 {
            page_tables[slot] = allocate_table();
            directory.set(slot, page_tables[slot], PRESENT | WRITABLE);
        }
        // SAFETY: as above.
        unsafe { table(page_tables[slot]) }.set(
            page_table::index(address, 0),
            base + offset,
            flags,
        );
    };
    for offset in (0..u64::from(image.size_of_headers())).step_by(PAGE_SIZE) {
        map(offset, PRESENT | execute_disable);
    }
    for section in image.sections() {
        let start = u64::from(section.virtual_address);
        let mut flags = PRESENT;
        if section.characteristics & pe::SCN_MEM_WRITE != 0 {
            flags |= WRITABLE;
        }
        if section.characteristics & pe::SCN_MEM_EXECUTE == 0 {
            flags |= execute_disable;
        }
        for offset in (start..start + u64::from(section.virtual_size)).step_by(PAGE_SIZE) {
            map(offset, flags);
        }
    }
}

/// Turns on `IA32_EFER.NXE` if the CPU has NX, and returns the XD bit to use,
/// or 0. `cpu::initialize` does the same, but only after the move.
fn enable_no_execute() -> u64 {
    if __cpuid(0x8000_0000).eax < 0x8000_0001 || __cpuid(0x8000_0001).edx & EXTENDED_EDX_NX == 0 {
        return 0;
    }
    // SAFETY: the CPU has NX, so `IA32_EFER.NXE` exists.
    unsafe { instructions::write_msr(IA32_EFER, instructions::read_msr(IA32_EFER) | EFER_NXE) };
    EXECUTE_DISABLE
}

/// A zeroed page from the boot heap, for a table.
fn allocate_table() -> u64 {
    let physical = boot_memory::allocate_pages(1).expect("boot heap exhausted");
    // SAFETY: a fresh page, mapped at its physical address until the move.
    unsafe {
        ptr::with_exposed_provenance_mut::<u8>(usize::try_from(physical).unwrap())
            .write_bytes(0, PAGE_SIZE);
    }
    physical
}

/// The table mapped at `address`.
///
/// # Safety
///
/// `address` maps a page table.
unsafe fn table(address: u64) -> &'static PageTable {
    // SAFETY: guaranteed by the caller.
    unsafe { &*ptr::with_exposed_provenance::<PageTable>(usize::try_from(address).unwrap()) }
}
