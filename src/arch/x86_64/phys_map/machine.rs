// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ops::Range;
use core::ptr::{self, NonNull};
use core::slice;
use core::sync::atomic::{self, AtomicU64, AtomicUsize, Ordering};

use x86_64::VirtAddr;
use x86_64::instructions::tlb;
use x86_64::registers::control::{Cr3, Cr3Flags};
use x86_64::structures::paging::{PageTableFlags, PhysFrame};

use super::flags::{
    DEVICE_PAGE, ENTRIES, KERNEL_PAGE, LARGE_PAGE_SIZE, PAGE_TABLE_SLOT, SLOT_SIZE, TABLE,
    UPPER_LEVELS, USER_PAGE, WIRED,
};
use super::phys_map::{BOOT_SPACE, CACHE, KERNEL_PHYS_MAP, NO_EXECUTE, PhysMap};
use super::table::{enter_kernel_page, entry_address, frame, index, table_entry, window_entry};
use crate::arch::x86_64::address::PhysAddr;
use crate::arch::x86_64::boot_memory;
use crate::arch::x86_64::memory_layout::{MAX_USER_ADDRESS, MIN_KERNEL_ADDRESS, PAGE_SIZE};
use crate::arch::x86_64::shootdown;
use crate::error::Error;
use crate::slab::CacheBox;
use crate::smp::{self, CpuMask};
use crate::sync::Spinlock;
use crate::vm::Protection;
use crate::vm::page;
use crate::vm::phys_map::{MachinePhysMap, Wiring};

impl MachinePhysMap for PhysMap {
    fn steal_memory(size: usize) -> *mut u8 {
        boot_memory::steal_pages(size.div_ceil(PAGE_SIZE)).direct_map_ptr()
    }

    fn kernel() -> &'static Self {
        KERNEL_PHYS_MAP.get().expect("pmap: not bootstrapped")
    }

    fn create() -> Result<&'static Self, Error> {
        let mut phys_map = CACHE.allocate(Self {
            pml4: PhysAddr(0),
            references: AtomicUsize::new(1),
            active_cpus: CpuMask::empty(),
            resident: AtomicUsize::new(0),
            lock: Spinlock::new(()),
        })?;
        // In no object, so at index 0.
        let pml4 = page::alloc::allocate_zeroed(0)
            .expect("pmap_create")
            .address();
        // SAFETY: the new table is zeroed and the physical map's alone; the
        // kernel's is read atomically, as the CPU sets its accessed bits.
        // Tables are arrays of entries, reached through the direct map.
        let (entries, kernel_entries) = unsafe {
            (
                slice::from_raw_parts_mut(pml4.direct_map_ptr::<u64>(), ENTRIES),
                slice::from_raw_parts(Self::kernel().pml4.direct_map_ptr::<AtomicU64>(), ENTRIES),
            )
        };
        // The kernel's entries past the window's are set once at boot.
        for (entry, kernel_entry) in entries[PAGE_TABLE_SLOT + 1..]
            .iter_mut()
            .zip(&kernel_entries[PAGE_TABLE_SLOT + 1..])
        {
            *entry = kernel_entry.load(Ordering::Relaxed);
        }
        let no_execute = *NO_EXECUTE.get().expect("pmap: not bootstrapped");
        entries[PAGE_TABLE_SLOT] = frame(pml4).as_u64() | (TABLE | no_execute).bits();
        phys_map.pml4 = pml4;
        let phys_map = CacheBox::into_raw(phys_map);
        // The box's provenance, which `destroy` takes back from a pointer
        // made from a shared borrow.
        phys_map.expose_provenance();
        // SAFETY: a physical map lives until pmap_destroy frees it, which
        // comes later.
        Ok(unsafe { phys_map.as_ref() })
    }

    fn reference(&self) {
        self.references.fetch_add(1, Ordering::Relaxed);
    }

    unsafe fn destroy(phys_map: NonNull<Self>) {
        // The box's own provenance, which `create` exposed.
        let phys_map = NonNull::<Self>::with_exposed_provenance(phys_map.addr());
        // SAFETY: the caller's reference keeps it allocated until its box
        // drops below.
        let this = unsafe { phys_map.as_ref() };
        if this.references.fetch_sub(1, Ordering::Release) != 1 {
            return;
        }
        // Every other holder's use comes before the free.
        atomic::fence(Ordering::Acquire);
        // Active on no CPU, it is in no TLB: no shootdown. The tables the
        // entries `slots` of `table` point at.
        let below = |table: PhysAddr, slots: Range<usize>| {
            slots.filter_map(move |slot| {
                let entry = table_entry(table, slot).load(Ordering::Relaxed);
                PageTableFlags::from_bits_truncate(entry)
                    .contains(PageTableFlags::PRESENT)
                    .then(|| entry_address(entry))
            })
        };
        let free = |table: PhysAddr| {
            page::alloc::free(
                page::alloc::lookup(table).expect("pmap_destroy: a table outside the page array"),
            );
        };
        // The user slots' tables, each after the tables below it, then the
        // PML4; kernel space's are shared.
        for directory_pointer in below(this.pml4, 0..MAX_USER_ADDRESS / SLOT_SIZE) {
            for directory in below(directory_pointer, 0..ENTRIES) {
                for table in below(directory, 0..ENTRIES) {
                    free(table);
                }
                free(directory);
            }
            free(directory_pointer);
        }
        free(this.pml4);
        // SAFETY: a `CACHE` box `create` gave up, with that box's
        // provenance; nothing uses it after.
        drop(unsafe { CacheBox::from_raw(&CACHE, phys_map) });
    }

    fn activate(&self) {
        // The kernel physical map stays active on every CPU; a user physical
        // map's bit goes before CR3, so a TLB shootdown finds this CPU.
        if !ptr::eq(self, Self::kernel()) {
            self.active_cpus.insert(smp::current().id());
        }
        // SAFETY: every PML4 maps kernel space as the kernel's does.
        unsafe {
            Cr3::write(
                PhysFrame::containing_address(frame(self.pml4)),
                Cr3Flags::empty(),
            );
        }
    }

    fn deactivate(&self) {
        // The kernel physical map stays active on every CPU.
        if ptr::eq(self, Self::kernel()) {
            return;
        }
        // After CR3, so a TLB shootdown finds this CPU until then.
        self.active_cpus.remove(smp::current().id());
    }

    fn enter(&self, address: usize, physical: PhysAddr, protection: Protection, wiring: Wiring) {
        if ptr::eq(self, Self::kernel()) {
            assert!(
                address >= MIN_KERNEL_ADDRESS,
                "pmap_enter: {address:#x} is not a kernel address"
            );
            assert!(
                page::alloc::lookup(physical).is_some(),
                "pmap_enter: memory outside the page array, uncached, not yet"
            );
            assert!(
                !protection.contains(Protection::EXECUTE),
                "pmap_enter: executable kernel memory, not yet"
            );
            let mut flags = PageTableFlags::PRESENT;
            if protection.contains(Protection::WRITE) {
                flags |= PageTableFlags::WRITABLE;
            }
            if matches!(wiring, Wiring::Wired) {
                flags |= WIRED;
            }
            assert!(
                Self::extract_kernel(address).is_none(),
                "pmap_enter: pmap_remove_range on the page mapped there, not yet"
            );
            self.resident.fetch_add(1, Ordering::Relaxed);
            enter_kernel_page(address, physical, flags);
            return;
        }
        assert!(
            address < MAX_USER_ADDRESS,
            "pmap_enter: {address:#x} is not a user address"
        );
        // Tables come before the lock, and go in under it where still missing.
        let mut spares = [None; UPPER_LEVELS.len()];
        let (tables, table) = loop {
            if let Err(missing) = self.expand(address, &mut []) {
                for spare in spares
                    .iter_mut()
                    .filter(|spare| spare.is_none())
                    .take(missing)
                {
                    // In no object, so at index 0.
                    *spare = Some(
                        page::alloc::allocate_zeroed(0)
                            .expect("pmap_expand: VM_PAGE_WAIT, not yet"),
                    );
                }
            }
            let tables = self.lock.lock();
            if let Ok(table) = self.expand(address, &mut spares) {
                break (tables, table);
            }
        };
        assert!(
            page::alloc::lookup(physical).is_some(),
            "pmap_enter: memory outside the page array, uncached, not yet"
        );
        // The page table's lock, before the physical map's drops.
        let entries = page::alloc::lookup(table)
            .expect("pmap_enter: a page table outside the page array")
            .table_lock
            .lock();
        drop(tables);
        let entry = table_entry(table, index(address, PAGE_SIZE));
        let old = entry.load(Ordering::Relaxed);
        let mut flags = USER_PAGE;
        if protection.contains(Protection::WRITE) {
            flags |= PageTableFlags::WRITABLE;
        }
        if !protection.contains(Protection::EXECUTE) {
            flags |= *NO_EXECUTE.get().expect("pmap: not bootstrapped");
        }
        if matches!(wiring, Wiring::Wired) {
            flags |= WIRED;
        }
        if old != 0 {
            // In place, the resident count kept: the same page keeps D; the
            // old page's accessed and dirty bits go, kept nowhere yet, as in
            // `remove`. Atomic, as the CPU sets them.
            let kept = if entry_address(old) == physical {
                PageTableFlags::DIRTY
            } else {
                PageTableFlags::empty()
            };
            entry.update(Ordering::SeqCst, Ordering::Relaxed, |value| {
                frame(physical).as_u64() | flags.bits() | (value & kept.bits())
            });
            // After the entry changes, as in `protect`.
            let current = smp::current().id();
            assert!(
                !self.active_cpus.contains_other(current),
                "PMAP_UPDATE_TLBS: signal_cpus, not yet"
            );
            if self.active_cpus.contains(current) {
                tlb::flush(VirtAddr::new(u64::try_from(address).unwrap()));
            }
        } else {
            self.resident.fetch_add(1, Ordering::Relaxed);
            // A page that was not present is in no TLB, so nothing to
            // invalidate.
            entry.store(frame(physical).as_u64() | flags.bits(), Ordering::Release);
        }
        drop(entries);
        for spare in spares.into_iter().flatten() {
            page::alloc::free(spare);
        }
    }

    fn change_wiring(&self, address: usize, wiring: Wiring) {
        assert!(
            matches!(wiring, Wiring::Pageable),
            "pmap_change_wiring: wiring down, not yet"
        );
        if !ptr::eq(self, Self::kernel()) {
            let tables = self.lock.lock();
            let table = self
                .expand(address, &mut [])
                .unwrap_or_else(|_| panic!("pmap_change_wiring: pte missing"));
            // The page table's lock, before the physical map's drops.
            let entries = page::alloc::lookup(table)
                .expect("pmap_change_wiring: a page table outside the page array")
                .table_lock
                .lock();
            drop(tables);
            // Atomic, as the CPU sets accessed and dirty bits; it ignores
            // this one, so no TLB needs flushing.
            table_entry(table, index(address, PAGE_SIZE))
                .fetch_and(!WIRED.bits(), Ordering::Relaxed);
            drop(entries);
            return;
        }
        assert!(
            address >= MIN_KERNEL_ADDRESS,
            "pmap_change_wiring: {address:#x} is not a kernel address"
        );
        // Kernel space's PML4 entries are always there.
        for level in [2, 1] {
            let entry = PageTableFlags::from_bits_truncate(
                window_entry(address, level).load(Ordering::Relaxed),
            );
            assert!(
                entry.contains(PageTableFlags::PRESENT)
                    && !entry.contains(PageTableFlags::HUGE_PAGE),
                "pmap_change_wiring: pte missing"
            );
        }
        // Atomic, as the CPU sets accessed and dirty bits; it ignores this
        // one, so no TLB needs flushing.
        window_entry(address, 0).fetch_and(!WIRED.bits(), Ordering::Relaxed);
    }

    fn remove(&self, range: Range<usize>) {
        if !ptr::eq(self, Self::kernel()) {
            let tables = self.lock.lock();
            let current = smp::current().id();
            let active = self.active_cpus.contains(current);
            let mut address = range.start;
            while address < range.end {
                // The end of the page table's span, or of the range.
                let next = address
                    .checked_add(LARGE_PAGE_SIZE)
                    .map_or(range.end, |next| {
                        (next & !(LARGE_PAGE_SIZE - 1)).min(range.end)
                    });
                // Page tables stay until the physical map goes.
                if let Ok(table) = self.expand(address, &mut []) {
                    let entries = page::alloc::lookup(table)
                        .expect("pmap_remove: a page table outside the page array")
                        .table_lock
                        .lock();
                    for page in (address..next).step_by(PAGE_SIZE) {
                        let entry = table_entry(table, index(page, PAGE_SIZE));
                        // Atomic, as the CPU sets accessed and dirty bits;
                        // they are kept nowhere yet.
                        if entry.swap(0, Ordering::SeqCst) != 0 {
                            self.resident.fetch_sub(1, Ordering::Relaxed);
                            if active {
                                tlb::flush(VirtAddr::new(u64::try_from(page).unwrap()));
                            }
                        }
                    }
                    drop(entries);
                }
                address = next;
            }
            // After the entries change, as in `protect`.
            assert!(
                !self.active_cpus.contains_other(current),
                "PMAP_UPDATE_TLBS: signal_cpus, not yet"
            );
            drop(tables);
            return;
        }
        assert!(
            range.start >= MIN_KERNEL_ADDRESS,
            "pmap_remove_range: {:#x} is not a kernel address",
            range.start
        );
        let mut address = range.start;
        while address < range.end {
            // The end of the page table's span, or of the range.
            let next = address
                .checked_add(LARGE_PAGE_SIZE)
                .map_or(range.end, |next| {
                    (next & !(LARGE_PAGE_SIZE - 1)).min(range.end)
                });
            // Kernel space's PML4 entries are always there; its directories
            // and page tables may not be.
            let table = [2, 1].into_iter().all(|level| {
                let entry = PageTableFlags::from_bits_truncate(
                    window_entry(address, level).load(Ordering::Relaxed),
                );
                assert!(
                    !entry.contains(PageTableFlags::PRESENT | PageTableFlags::HUGE_PAGE),
                    "pmap_remove: a large page, not yet"
                );
                entry.contains(PageTableFlags::PRESENT)
            });
            if table {
                for page in (address..next).step_by(PAGE_SIZE) {
                    // Atomic, as the CPU sets accessed and dirty bits; they
                    // are kept nowhere yet. A SeqCst RMW: `shoot` reads its
                    // targets after it, as a joining CPU flushes after its
                    // join.
                    if window_entry(page, 0).swap(0, Ordering::SeqCst) != 0 {
                        self.resident.fetch_sub(1, Ordering::Relaxed);
                    }
                }
            }
            address = next;
        }
        // After the swaps: every CPU using the kernel's tables drops them.
        shootdown::shoot(range);
    }

    fn protect(&self, range: Range<usize>, protection: Protection) {
        let no_execute = *NO_EXECUTE.get().expect("pmap: not bootstrapped");
        // Nothing to lower: writing stays, and running too or the CPU can't
        // refuse it.
        if protection.contains(Protection::READ | Protection::WRITE)
            && (protection.contains(Protection::EXECUTE) || no_execute.is_empty())
        {
            return;
        }
        assert!(
            !ptr::eq(self, Self::kernel()),
            "pmap_protect: the kernel pmap, not yet"
        );
        // Not even reading: the mappings go.
        if !protection.contains(Protection::READ) {
            return self.remove(range);
        }
        // Rights only go: writing, and running as no-execute.
        let clear = if protection.contains(Protection::WRITE) {
            PageTableFlags::empty()
        } else {
            PageTableFlags::WRITABLE
        };
        let set = if protection.contains(Protection::EXECUTE) {
            PageTableFlags::empty()
        } else {
            no_execute
        };
        let tables = self.lock.lock();
        let current = smp::current().id();
        let active = self.active_cpus.contains(current);
        let mut address = range.start;
        while address < range.end {
            // The end of the page table's span, or of the range.
            let next = address
                .checked_add(LARGE_PAGE_SIZE)
                .map_or(range.end, |next| {
                    (next & !(LARGE_PAGE_SIZE - 1)).min(range.end)
                });
            if let Ok(table) = self.expand(address, &mut []) {
                let entries = page::alloc::lookup(table)
                    .expect("pmap_protect: a page table outside the page array")
                    .table_lock
                    .lock();
                for page in (address..next).step_by(PAGE_SIZE) {
                    let entry = table_entry(table, index(page, PAGE_SIZE));
                    // Only the table's lock holder changes rights.
                    let flags = PageTableFlags::from_bits_truncate(entry.load(Ordering::Relaxed));
                    if !flags.contains(PageTableFlags::PRESENT)
                        || (!flags.intersects(clear) && flags.contains(set))
                    {
                        continue;
                    }
                    // Atomic, as the CPU sets accessed and dirty bits.
                    entry.update(Ordering::SeqCst, Ordering::Relaxed, |value| {
                        (value & !clear.bits()) | set.bits()
                    });
                    if active {
                        tlb::flush(VirtAddr::new(u64::try_from(page).unwrap()));
                    }
                }
                drop(entries);
            }
            address = next;
        }
        // After the entries change, as `activate` sets its bit before CR3,
        // and `deactivate` clears it after.
        assert!(
            !self.active_cpus.contains_other(current),
            "PMAP_UPDATE_TLBS: signal_cpus, not yet"
        );
        drop(tables);
    }

    fn resident_count(&self) -> usize {
        self.resident.load(Ordering::Relaxed)
    }

    fn enter_kernel(address: usize, physical: PhysAddr) {
        enter_kernel_page(address, physical, KERNEL_PAGE);
    }

    fn extract_kernel(address: usize) -> Option<PhysAddr> {
        // Kernel space's PML4 entries are always there.
        for level in [2, 1] {
            let entry = PageTableFlags::from_bits_truncate(
                window_entry(address, level).load(Ordering::Relaxed),
            );
            if !entry.contains(PageTableFlags::PRESENT) || entry.contains(PageTableFlags::HUGE_PAGE)
            {
                return None;
            }
        }
        let entry = window_entry(address, 0).load(Ordering::Relaxed);
        PageTableFlags::from_bits_truncate(entry)
            .contains(PageTableFlags::PRESENT)
            .then(|| {
                let frame = x86_64::PhysAddr::new_truncate(entry).as_u64();
                PhysAddr(usize::try_from(frame).unwrap()).align_down()
            })
    }

    fn enter_device(address: Range<usize>, physical: PhysAddr) {
        for offset in (0..address.end - address.start).step_by(PAGE_SIZE) {
            enter_kernel_page(address.start + offset, physical + offset, DEVICE_PAGE);
        }
    }

    fn boot_space() -> Range<usize> {
        BOOT_SPACE.get().expect("pmap: not bootstrapped").clone()
    }

    fn init() {
        CACHE.register();
    }
}
