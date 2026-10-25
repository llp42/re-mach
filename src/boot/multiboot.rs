// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from DragonFly BSD
// Upstream-Path: stand/boot/efi/loader/bootinfo.c
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 1998 Michael Smith <msmith@freebsd.org>
// SPDX-FileCopyrightText: 2004, 2006 Marcel Moolenaar
// SPDX-FileCopyrightText: 2014 The FreeBSD Foundation
//! Translates a Multiboot loader's information into the kernel handoff and
//! enters the kernel.
//!
//! This runs at the kernel's linked address under the entry page tables,
//! before the kernel's console exists: a panic would end silently, so
//! failures are reported with `diagnostics::fatal` and the code avoids
//! operations that panic. The loader's structures are read through the
//! identity mapping below `KERNEL_LIMIT`.
//!
//! The first module is the init program. The environment and metadata are
//! written above everything the loader placed, and `kernel_end` follows
//! them; the kernel keeps all of it reserved and overwrites the memory from
//! `kernel_end` on with its first page tables.

use super::modinfo::{self, Environment, Metadata};
use crate::arch::entry::{self, KERNEL_BASE, KERNEL_LIMIT, LOAD_ADDRESS};
use crate::arch::{ELF_MACHINE, PAGE_SIZE};
use crate::diagnostics::{self, dbg_println};

/// Path the kernel reports as its own name.
const KERNEL_NAME: &str = "/boot/kernel";
const INIT_NAME: &str = "/boot/init";
const INIT_TYPE: &str = "init";
const ENVIRONMENT_CAPACITY: u64 = PAGE_SIZE;
/// The records besides the memory map.
const RECORDS_CAPACITY: u64 = PAGE_SIZE;
/// Free memory above `kernel_end` that the kernel's first page tables
/// overwrite before switching to them. They take about 5 MiB with 1 GiB of
/// memory and grow by about one page per 6 MiB more.
const SCRATCH_LEN: u64 = 16 << 20;

const INFO_SIZE: u64 = 52;
const HAS_MODULES: u32 = 1 << 3;
const HAS_MEMORY_MAP: u32 = 1 << 6;
const MODULE_SIZE: u64 = 16;
const MAP_ENTRY_SIZE: usize = 20;
const USABLE: u32 = 1;

const ELF_HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const LOADABLE: u32 = 1;

unsafe extern "C" {
    /// End of the kernel image, including its zero-initialized data.
    #[link_name = "end"]
    static IMAGE_END: u8;
}

/// Called by the entry code with the loader's information address; never
/// returns.
pub(crate) extern "C" fn enter(info: u64) -> ! {
    diagnostics::init();
    diagnostics::print(format_args!("re-mach: multiboot entry\r\n"));
    check_layout();

    let image_end = (&raw const IMAGE_END) as u64 - KERNEL_BASE;
    let header = physical(info, INFO_SIZE, "boot information");
    let flags = read_u32(header, 0);
    if flags & HAS_MEMORY_MAP == 0 {
        diagnostics::fatal(format_args!("boot information: no memory map"));
    }
    if flags & HAS_MODULES == 0 || read_u32(header, 20) == 0 {
        diagnostics::fatal(format_args!("init module: none"));
    }
    let map_address = u64::from(read_u32(header, 48));
    let map = physical(map_address, u64::from(read_u32(header, 44)), "memory map");
    let modules_address = u64::from(read_u32(header, 24));
    let module_count = u64::from(read_u32(header, 20));
    let modules = physical(modules_address, module_count * MODULE_SIZE, "module list");

    let init_start = u64::from(read_u32(modules, 0));
    let init_end = u64::from(read_u32(modules, 4));
    if init_start < image_end || init_end <= init_start {
        diagnostics::fatal(format_args!(
            "init module: {init_start:#x}..{init_end:#x} is not above the kernel"
        ));
    }
    let mut loader_end = image_end
        .max(info + INFO_SIZE)
        .max(map_address + map.len() as u64)
        .max(modules_address + modules.len() as u64);
    for module in modules.chunks_exact(MODULE_SIZE as usize) {
        loader_end = loader_end.max(u64::from(read_u32(module, 4)));
    }

    let entries = map_entries(map);
    let entry_count = entries.clone().count() as u64;
    let environment_start = loader_end.next_multiple_of(PAGE_SIZE);
    let metadata_start = environment_start + ENVIRONMENT_CAPACITY;
    let metadata_len =
        (RECORDS_CAPACITY + entry_count * MAP_ENTRY_SIZE as u64).next_multiple_of(PAGE_SIZE);
    let kernel_end = metadata_start + metadata_len;
    let needed = environment_start..kernel_end + SCRATCH_LEN;
    let usable = entries.clone().any(|(base, length, kind)| {
        kind == USABLE && base <= needed.start && needed.end <= base.saturating_add(length)
    });
    if !usable || needed.end > KERNEL_LIMIT {
        diagnostics::fatal(format_args!(
            "handoff memory: {:#x}..{:#x} is not usable memory below {KERNEL_LIMIT:#x}",
            needed.start, needed.end
        ));
    }
    dbg_println!(
        "kernel: {:#x}..{:#x}; init {:#x}..{:#x}; environment {:#x}; metadata {:#x}; end {:#x}",
        LOAD_ADDRESS,
        image_end,
        init_start,
        init_end,
        environment_start,
        metadata_start,
        kernel_end
    );

    // SAFETY: the range is usable memory below KERNEL_LIMIT, identity
    // mapped, above everything the loader placed and the kernel image, so
    // nothing else uses it. It is initialized before forming the slices.
    let (environment, metadata) = unsafe {
        let start = environment_start as *mut u8;
        start.write_bytes(0, (kernel_end - environment_start) as usize);
        (
            core::slice::from_raw_parts_mut(start, ENVIRONMENT_CAPACITY as usize),
            core::slice::from_raw_parts_mut(metadata_start as *mut u8, metadata_len as usize),
        )
    };

    let mut env = Environment::new(environment);
    require(
        env.set("kernelname", format_args!("{KERNEL_NAME}")),
        "kernel environment",
    );
    require(
        env.set("vfs.root.mountfrom", format_args!("devfs:")),
        "kernel environment",
    );
    require(env.finish(), "kernel environment");

    let mut records = Metadata::new(metadata);
    require(records.string(modinfo::NAME, KERNEL_NAME), "metadata");
    require(records.string(modinfo::TYPE, "elf kernel"), "metadata");
    require(records.u64(modinfo::ADDRESS, LOAD_ADDRESS), "metadata");
    require(
        records.u64(modinfo::SIZE, image_end - LOAD_ADDRESS),
        "metadata",
    );
    require(
        records.u32(modinfo::BOOT_FLAGS, modinfo::SERIAL_CONSOLE),
        "metadata",
    );
    require(
        records.u64(modinfo::ENVIRONMENT, environment_start),
        "metadata",
    );
    let map_record = require(
        records.record(modinfo::MEMORY_MAP, entry_count as usize * MAP_ENTRY_SIZE),
        "memory map record",
    );
    for ((base, length, kind), target) in entries.zip(map_record.chunks_exact_mut(MAP_ENTRY_SIZE)) {
        target[0..8].copy_from_slice(&base.to_le_bytes());
        target[8..16].copy_from_slice(&length.to_le_bytes());
        target[16..20].copy_from_slice(&kind.to_le_bytes());
    }
    require(records.string(modinfo::NAME, INIT_NAME), "metadata");
    require(records.string(modinfo::TYPE, INIT_TYPE), "metadata");
    require(records.u64(modinfo::ADDRESS, init_start), "metadata");
    require(
        records.u64(modinfo::SIZE, init_end - init_start),
        "metadata",
    );
    require(records.finish(), "metadata");

    diagnostics::print(format_args!("re-mach: entering kernel\r\n"));
    // SAFETY: the entry page tables are active; the image, environment and
    // metadata are complete; the memory from kernel_end on was checked to
    // be usable and holds nothing in use. Both addresses are below
    // KERNEL_LIMIT and fit 32 bits.
    unsafe { entry::enter_kernel(metadata_start as u32, kernel_end as u32) }
}

/// Stops unless every loadable segment of the kernel image lies at
/// `LOAD_ADDRESS` plus its file offset, where the loader put it.
fn check_layout() {
    // SAFETY: the ELF header and program headers are the start of the image,
    // which the loader placed at LOAD_ADDRESS, mapped at KERNEL_BASE.
    let image = unsafe {
        core::slice::from_raw_parts(
            (KERNEL_BASE + LOAD_ADDRESS) as *const u8,
            PAGE_SIZE as usize,
        )
    };
    let program_headers = usize::try_from(read_u64(image, 32)).unwrap_or(usize::MAX);
    let count = usize::from(read_u16(image, 56));
    let valid = image[..4] == [0x7f, b'E', b'L', b'F']
        && read_u16(image, 18) == ELF_MACHINE
        && usize::from(read_u16(image, 54)) == PROGRAM_HEADER_SIZE
        && program_headers >= ELF_HEADER_SIZE
        && program_headers
            .checked_add(count * PROGRAM_HEADER_SIZE)
            .is_some_and(|end| end <= image.len());
    if !valid {
        diagnostics::fatal(format_args!("kernel layout: unreadable ELF header"));
    }
    for index in 0..count {
        let header = &image[program_headers + index * PROGRAM_HEADER_SIZE..][..PROGRAM_HEADER_SIZE];
        if read_u32(header, 0) != LOADABLE {
            continue;
        }
        let offset = read_u64(header, 8);
        let address = read_u64(header, 16);
        if address.wrapping_sub(KERNEL_BASE) != LOAD_ADDRESS.wrapping_add(offset) {
            diagnostics::fatal(format_args!(
                "kernel layout: segment at {address:#x} is not at load address plus offset {offset:#x}"
            ));
        }
    }
}

/// `len` bytes of loader memory at physical `start`, which must be nonzero
/// and end below `KERNEL_LIMIT`.
fn physical(start: u64, len: u64, what: &str) -> &'static [u8] {
    if start == 0 || start.checked_add(len).is_none_or(|end| end > KERNEL_LIMIT) {
        diagnostics::fatal(format_args!(
            "{what}: {start:#x} + {len:#x} is outside the identity map"
        ));
    }
    // SAFETY: the range is identity mapped and holds the loader's
    // structures, which nothing writes while the boot code reads them.
    unsafe { core::slice::from_raw_parts(start as *const u8, len as usize) }
}

/// Entries of the loader's memory map as `(base, length, kind)`.
/// Each entry is a 4-byte size that excludes itself, then base, length
/// and kind; truncated entries end the map.
fn map_entries(map: &'static [u8]) -> impl Iterator<Item = (u64, u64, u32)> + Clone {
    let mut offset = 0_usize;
    core::iter::from_fn(move || {
        let size = map.get(offset..offset + 4).map(|_| read_u32(map, offset))? as usize;
        let entry = map.get(offset + 4..(offset + 4).checked_add(size)?)?;
        if size < MAP_ENTRY_SIZE {
            return None;
        }
        offset += 4 + size;
        Some((read_u64(entry, 0), read_u64(entry, 8), read_u32(entry, 16)))
    })
}

fn require<T, E: core::fmt::Debug>(result: Result<T, E>, stage: &str) -> T {
    result.unwrap_or_else(|error| diagnostics::fatal(format_args!("{stage}: {error:?}")))
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    let mut value = [0; 4];
    value.copy_from_slice(&bytes[offset..offset + 4]);
    u32::from_le_bytes(value)
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    let mut value = [0; 8];
    value.copy_from_slice(&bytes[offset..offset + 8]);
    u64::from_le_bytes(value)
}
