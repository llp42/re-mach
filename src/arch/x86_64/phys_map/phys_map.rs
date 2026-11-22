// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ops::Range;
use core::sync::atomic::{AtomicUsize, Ordering};

use x86_64::registers::control::{Cr3, Cr3Flags, Cr4, Cr4Flags};
use x86_64::registers::model_specific::{Efer, EferFlags};
use x86_64::structures::paging::{PageTableFlags, PhysFrame};

use super::flags::{
    DIRECT_MAP_SLOT, DIRECTORY_SIZE, KERNEL_SLOT, LARGE_PAGE_SIZE, MIN_DIRECT_MAP_SIZE,
    PAGE_TABLE_SLOT, SLOT_SIZE, TABLE, UPPER_LEVELS, USER_TABLE, set_page_attributes,
};
use super::table::{
    alloc_tables, entries, entry_address, frame, index, map_direct, map_kernel_space, table_entry,
};
use crate::arch::x86_64::address::PhysAddr;
use crate::arch::x86_64::boot_memory;
use crate::arch::x86_64::cpuid;
use crate::arch::x86_64::memory_layout::{DIRECT_MAP_BASE, KERNEL_BASE};
use crate::slab::Cache;
use crate::smp::CpuMask;
use crate::sync::{OnceLock, Spinlock};
use crate::vm::page::page::Page;

/// One address space's page tables.
pub struct PhysMap {
    /// Its PML4 table.
    pub(super) pml4: PhysAddr,
    /// References held on it.
    pub(super) references: AtomicUsize,
    /// The CPUs it is active on.
    pub(super) active_cpus: CpuMask,
    /// How many pages it maps.
    pub(super) resident: AtomicUsize,
    /// Its lock: tables are linked into it only under it.
    pub(super) lock: Spinlock<()>,
}

impl PhysMap {
    /// Its PML4 table.
    pub(in crate::arch::x86_64) const fn pml4(&self) -> PhysAddr {
        self.pml4
    }

    /// The page table that maps user `address`, found from the PML4 through
    /// the direct map, a table from `spares` linked in where one is missing;
    /// only the lock's holder passes any. `Err` with how many are missing
    /// once `spares` run out.
    pub(super) fn expand(
        &self,
        address: usize,
        spares: &mut [Option<&'static Page>],
    ) -> Result<PhysAddr, usize> {
        let mut table = self.pml4;
        for (level, size) in UPPER_LEVELS.into_iter().enumerate() {
            let entry = table_entry(table, index(address, size));
            let mut value = entry.load(Ordering::Acquire);
            if !PageTableFlags::from_bits_truncate(value).contains(PageTableFlags::PRESENT) {
                let spare = spares
                    .iter_mut()
                    .find_map(Option::take)
                    .ok_or(UPPER_LEVELS.len() - level)?;
                value = frame(spare.address()).as_u64() | USER_TABLE.bits();
                // After the table's zeroing.
                entry.store(value, Ordering::Release);
            }
            table = entry_address(value);
        }
        Ok(table)
    }
}

/// The cache physical maps come from.
pub(super) static CACHE: Cache<PhysMap> = Cache::new("pmap");

/// The kernel's physical map, once `bootstrap` builds it.
pub(super) static KERNEL_PHYS_MAP: OnceLock<PhysMap> = OnceLock::new();

/// The kernel addresses the boot mappings use, once `bootstrap` maps them.
pub(super) static BOOT_SPACE: OnceLock<Range<usize>> = OnceLock::new();

/// End of the direct map, once `bootstrap` sets it.
static DIRECT_MAP_END: OnceLock<usize> = OnceLock::new();

/// The no-execute bit, empty where the CPU has none, once `bootstrap`
/// probes it.
pub(super) static NO_EXECUTE: OnceLock<PageTableFlags> = OnceLock::new();

/// Builds the kernel's page tables and physical map, switches to them with
/// EFER.NXE set where the CPU has NX, and sets the page attribute table.
///
/// # Panics
///
/// If boot memory runs out, physical memory goes past the direct map, or
/// on a second call.
///
/// # Safety
///
/// Runs once, on the boot CPU, after boot memory's bootstrap.
pub unsafe fn bootstrap() {
    let image_start = PhysAddr((&raw const crate::arch::x86_64::boot_start).addr());
    let text_start = PhysAddr((&raw const crate::arch::x86_64::text_start).addr() - KERNEL_BASE);
    let text_end = PhysAddr((&raw const crate::arch::x86_64::text_end).addr() - KERNEL_BASE);
    let rodata_end = PhysAddr((&raw const crate::arch::x86_64::rodata_end).addr() - KERNEL_BASE);
    let image_end = PhysAddr((&raw const crate::arch::x86_64::_end).addr() - KERNEL_BASE);
    let (heap_end, memory_end) = (boot_memory::heap_end(), boot_memory::memory_end());
    let boot_end = image_end.max(heap_end).0.next_multiple_of(LARGE_PAGE_SIZE);
    let direct_map_size = memory_end
        .0
        .next_multiple_of(DIRECTORY_SIZE)
        .max(MIN_DIRECT_MAP_SIZE);
    assert!(
        direct_map_size <= (KERNEL_SLOT - DIRECT_MAP_SLOT) * SLOT_SIZE,
        "pmap: physical memory past the direct map"
    );
    let has_execute_disable = cpuid::cpu_id()
        .get_extended_processor_and_feature_identifiers()
        .is_some_and(|info| info.has_execute_disable());
    let mut no_execute = PageTableFlags::empty();
    no_execute.set(PageTableFlags::NO_EXECUTE, has_execute_disable);
    let pml4 = alloc_tables(1);
    // SAFETY: guaranteed by the caller; the PML4 was just allocated.
    unsafe {
        map_kernel_space(
            pml4,
            image_start..image_end,
            text_start..text_end,
            text_end..rodata_end,
            PhysAddr(boot_end),
            no_execute,
        );
        map_direct(pml4, direct_map_size, no_execute);
        entries(pml4, 1)[PAGE_TABLE_SLOT].set_addr(frame(pml4), TABLE | no_execute);
    }
    let kernel_phys_map = PhysMap {
        pml4,
        references: AtomicUsize::new(1),
        active_cpus: CpuMask::all(),
        resident: AtomicUsize::new(0),
        lock: Spinlock::new(()),
    };
    assert!(
        KERNEL_PHYS_MAP.set(kernel_phys_map).is_ok()
            && BOOT_SPACE.set(KERNEL_BASE..KERNEL_BASE + boot_end).is_ok()
            && DIRECT_MAP_END
                .set(DIRECT_MAP_BASE + direct_map_size)
                .is_ok()
            && NO_EXECUTE.set(no_execute).is_ok(),
        "pmap: bootstrapped twice"
    );
    let pml4_frame = PhysFrame::containing_address(frame(pml4));
    // SAFETY: the new tables map the kernel window up to `boot_end`, which
    // holds all the kernel reaches, and all it runs from here on lies in
    // `text_start..text_end`; nothing uses the identity map any more. NXE
    // where the CPU has NX, before the tables using it load; global pages
    // where it has them.
    unsafe {
        Efer::update(|flags| flags.set(EferFlags::NO_EXECUTE_ENABLE, has_execute_disable));
        Cr4::update(|flags| flags.set(Cr4Flags::PAGE_GLOBAL, cpuid::features().has_pge()));
        Cr3::write(pml4_frame, Cr3Flags::empty());
    }
    // SAFETY: guaranteed by the caller; no mapping uses PAT entries 4 to 7.
    unsafe { set_page_attributes() };
}
