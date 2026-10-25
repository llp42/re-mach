// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from DragonFly BSD
// Upstream-Path: sys/kern/imgact_elf.c
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2000 David O'Brien
// SPDX-FileCopyrightText: 1995-1996 Søren Schmidt
// SPDX-FileCopyrightText: 1996 Peter Wemm
//! Starts the first user process from the init program the boot loader
//! passed as a module, instead of executing a file from the root
//! filesystem.
//!
//! The program is a static ELF64 executable, little endian, for the
//! architecture's machine, linked at fixed user addresses between the first
//! page and the user stack. Each loadable segment is mapped at its address
//! as zeroed memory, filled with its file contents, and given its
//! protection. The program starts with no arguments, environment or open
//! files. Any failure is fatal: there is no other process to run.

use core::ffi::{CStr, c_int, c_void};

use crate::arch::{ELF_MACHINE, PAGE_SIZE};
use crate::ffi;

/// Module type of the init program in the boot loader's metadata.
const MODULE_TYPE: &CStr = c"init";
const MODULE_ADDRESS: c_int = 0x0003;
const MODULE_SIZE: c_int = 0x0004;

const HEADER_SIZE: usize = 64;
const PROGRAM_HEADER_SIZE: usize = 56;
const MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];
const CLASS_64: u8 = 2;
const LITTLE_ENDIAN: u8 = 1;
const CURRENT_VERSION: u8 = 1;
const EXECUTABLE: u16 = 2;
const LOADABLE: u32 = 1;
const SEGMENT_EXECUTE: u32 = 1;
const SEGMENT_WRITE: u32 = 2;
const SEGMENT_READ: u32 = 4;
const PROTECT_READ: c_int = 1;
const PROTECT_WRITE: c_int = 2;
const PROTECT_EXECUTE: c_int = 4;
/// Room for a zero argument count and null argument and environment lists
/// at the top of the fresh, zeroed stack.
const START_FRAME: u64 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Error {
    Truncated,
    NotElf64,
    UnsupportedMachine,
    NotExecutable,
    InvalidSegment,
    NoSegments,
    InvalidEntry,
}

/// A validated init executable.
struct Program<'a> {
    image: &'a [u8],
    program_headers: &'a [u8],
    entry: u64,
}

/// One loadable segment: `data` belongs at `address`, followed by zeros up
/// to `memory_len` bytes.
struct Segment<'a> {
    address: u64,
    data: &'a [u8],
    memory_len: u64,
    flags: u32,
}

/// Runs in the init process, by the fork handler glue.c installs, when the
/// process is first scheduled. Returning enters user mode at the program's
/// entry point.
#[unsafe(no_mangle)]
pub extern "C" fn rust_start_init(_arg: *mut c_void, _frame: *mut c_void) {
    // SAFETY: the glue returns a constant.
    let stack_top = unsafe { ffi::glue_user_stack_top() };
    let program = Program::parse(init_module(), stack_top)
        .unwrap_or_else(|error| panic!("init: invalid program: {error:?}"));

    // SAFETY: this runs in the init process before it first enters user
    // mode, with nothing else using its descriptors or address space.
    unsafe {
        check(ffi::glue_init_root(), "root directory");
        check(ffi::glue_init_vmspace(), "address space");
    }
    for segment in program.segments(stack_top) {
        load(&segment);
    }
    // SAFETY: the segments are loaded and the entry point lies in an
    // executable one; the stack frame is in the fresh stack mapping.
    unsafe { ffi::glue_user_enter(program.entry, stack_top - START_FRAME) };
}

/// The init program's bytes, which the kernel keeps reserved.
fn init_module() -> &'static [u8] {
    // SAFETY: the type is a NUL-terminated string. A found module's address
    // and size records hold 8-byte values, possibly unaligned.
    let (address, size) = unsafe {
        let module = ffi::preload_search_by_type(MODULE_TYPE.as_ptr());
        if module.is_null() {
            panic!("init: no {MODULE_TYPE:?} module");
        }
        let address = ffi::preload_search_info(module, MODULE_ADDRESS);
        let size = ffi::preload_search_info(module, MODULE_SIZE);
        if address.is_null() || size.is_null() {
            panic!("init: module without address or size");
        }
        (
            address.cast::<u64>().read_unaligned(),
            size.cast::<u64>().read_unaligned(),
        )
    };
    if address == 0 || size == 0 {
        panic!("init: empty module");
    }
    // SAFETY: the boot loader placed the module below the kernel's first
    // free page, which the kernel keeps mapped at the relocated address and
    // never reuses; nothing writes it.
    unsafe { core::slice::from_raw_parts(address as *const u8, size as usize) }
}

/// Maps one segment, copies its file contents and sets its protection.
fn load(segment: &Segment<'_>) {
    let start = segment.address & !(PAGE_SIZE - 1);
    let end = (segment.address + segment.memory_len).next_multiple_of(PAGE_SIZE);
    let mut protection = 0;
    for (flag, bit) in [
        (SEGMENT_READ, PROTECT_READ),
        (SEGMENT_WRITE, PROTECT_WRITE),
        (SEGMENT_EXECUTE, PROTECT_EXECUTE),
    ] {
        if segment.flags & flag != 0 {
            protection |= bit;
        }
    }
    // SAFETY: `start..end` is page aligned and validated to lie between the
    // first page and the stack. copyout checks the user range itself and
    // reads `segment.data` within its length.
    unsafe {
        check(ffi::glue_user_map(start, end - start), "map segment");
        check(
            ffi::copyout(
                segment.data.as_ptr().cast(),
                segment.address as *mut c_void,
                segment.data.len(),
            ),
            "copy segment",
        );
        check(
            ffi::glue_user_protect(start, end - start, protection),
            "protect segment",
        );
    }
}

fn check(error: c_int, stage: &str) {
    if error != 0 {
        panic!("init: {stage}: error {error}");
    }
}

impl<'a> Program<'a> {
    /// Checks the header and every loadable segment. Segments must be in
    /// ascending order without sharing pages, lie between the first page
    /// and `stack_top`, and the entry point must lie in the file contents
    /// of an executable segment.
    fn parse(image: &'a [u8], stack_top: u64) -> Result<Self, Error> {
        let header = image.get(..HEADER_SIZE).ok_or(Error::Truncated)?;
        if header[..4] != MAGIC
            || header[4] != CLASS_64
            || header[5] != LITTLE_ENDIAN
            || header[6] != CURRENT_VERSION
            || usize::from(read_u16(header, 54)) != PROGRAM_HEADER_SIZE
        {
            return Err(Error::NotElf64);
        }
        if read_u16(header, 18) != ELF_MACHINE {
            return Err(Error::UnsupportedMachine);
        }
        if read_u16(header, 16) != EXECUTABLE {
            return Err(Error::NotExecutable);
        }
        let offset = usize::try_from(read_u64(header, 32)).map_err(|_| Error::Truncated)?;
        let len = usize::from(read_u16(header, 56)) * PROGRAM_HEADER_SIZE;
        let end = offset.checked_add(len).ok_or(Error::Truncated)?;
        let program_headers = image.get(offset..end).ok_or(Error::Truncated)?;
        let entry = read_u64(header, 24);

        let mut mapped_end = PAGE_SIZE;
        let mut entry_found = false;
        let mut segments = 0_usize;
        for program_header in program_headers.chunks_exact(PROGRAM_HEADER_SIZE) {
            let Some(segment) = segment(image, program_header, stack_top)? else {
                continue;
            };
            if segment.address & !(PAGE_SIZE - 1) < mapped_end {
                return Err(Error::InvalidSegment);
            }
            if segment.flags & SEGMENT_EXECUTE != 0
                && (segment.address..segment.address + segment.data.len() as u64).contains(&entry)
            {
                entry_found = true;
            }
            mapped_end = (segment.address + segment.memory_len).next_multiple_of(PAGE_SIZE);
            segments += 1;
        }
        if segments == 0 {
            return Err(Error::NoSegments);
        }
        if !entry_found {
            return Err(Error::InvalidEntry);
        }
        Ok(Self {
            image,
            program_headers,
            entry,
        })
    }

    /// Loadable segments in ascending order.
    fn segments(&self, stack_top: u64) -> impl Iterator<Item = Segment<'a>> + use<'a> {
        let image = self.image;
        let program_headers = self.program_headers;
        // `parse` rejected every invalid program header, so no error
        // remains to report.
        program_headers
            .chunks_exact(PROGRAM_HEADER_SIZE)
            .filter_map(move |header| segment(image, header, stack_top).ok().flatten())
    }
}

/// Decodes one program header; other kinds than loadable yield `None`.
fn segment<'a>(
    image: &'a [u8],
    header: &[u8],
    stack_top: u64,
) -> Result<Option<Segment<'a>>, Error> {
    if read_u32(header, 0) != LOADABLE {
        return Ok(None);
    }
    let offset = read_u64(header, 8);
    let address = read_u64(header, 16);
    let file_len = read_u64(header, 32);
    let memory_len = read_u64(header, 40);
    if file_len > memory_len || address < PAGE_SIZE {
        return Err(Error::InvalidSegment);
    }
    address
        .checked_add(memory_len)
        .and_then(|end| end.checked_next_multiple_of(PAGE_SIZE))
        .filter(|end| *end <= stack_top)
        .ok_or(Error::InvalidSegment)?;
    let data = usize::try_from(offset)
        .ok()
        .zip(usize::try_from(file_len).ok())
        .and_then(|(offset, len)| image.get(offset..offset.checked_add(len)?))
        .ok_or(Error::Truncated)?;
    Ok(Some(Segment {
        address,
        data,
        memory_len,
        flags: read_u32(header, 4),
    }))
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
