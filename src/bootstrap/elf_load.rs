// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! ELF images: what a program asks to have loaded, read in place.

use elf::ParseError;
use elf::abi;
use elf::endian::LittleEndian;
use elf::file::{Class, Elf64_Ehdr, FileHeader};
use elf::parse::ParseAt as _;
use elf::segment::ProgramHeader;

use crate::vm::Protection;

/// Where a position-independent image goes: 1 GiB, not GNU Mach's 128 MiB.
const PIE_BASE: usize = 0x4000_0000;

/// Why an image can't be loaded.
pub enum Error {
    /// A read past the image's end.
    OutOfBounds,
    /// Not an executable format known here.
    NotExecutable,
    /// An executable for another machine.
    WrongArchitecture,
    /// A known executable, but mangled.
    Corrupt,
}

impl From<ParseError> for Error {
    fn from(_: ParseError) -> Self {
        Self::Corrupt
    }
}

// GNU Mach's numbers: boot_read's -1, then exec.h's.
impl From<Error> for i32 {
    fn from(error: Error) -> Self {
        match error {
            Error::OutOfBounds => -1,
            Error::NotExecutable => 6000,
            Error::WrongArchitecture => 6001,
            Error::Corrupt => 6002,
        }
    }
}

bitflags::bitflags! {
    /// What a segment asks for, as GNU Mach's exec interface numbers it.
    pub struct SectionType: u32 {
        /// Readable.
        const READ = Protection::READ.bits();
        /// Writable.
        const WRITE = Protection::WRITE.bits();
        /// Executable.
        const EXECUTE = Protection::EXECUTE.bits();
        /// Memory to allocate.
        const ALLOCATE = 0x100;
        /// Contents from the file.
        const LOAD = 0x200;
    }
}

/// The protection a program header's `PF_*` `flags` ask for.
fn protection(flags: u32) -> Protection {
    let mut protection = Protection::empty();
    protection.set(Protection::READ, flags & abi::PF_R != 0);
    protection.set(Protection::WRITE, flags & abi::PF_W != 0);
    protection.set(Protection::EXECUTE, flags & abi::PF_X != 0);
    protection
}

/// A `PT_LOAD` segment: where its bytes are in the image, where it goes,
/// and what it asks for.
pub struct Segment {
    /// Its offset in the image.
    pub offset: usize,
    /// Its bytes in the image.
    pub file_size: usize,
    /// Where it goes, rebased.
    pub address: usize,
    /// Its bytes in memory: the file's, then zeroes.
    pub memory_size: usize,
    /// What it asks for.
    pub section_type: SectionType,
}

/// The program-header table, in place.
struct ProgramHeaders<'a> {
    /// Its bytes.
    table: &'a [u8],
    /// Bytes from one header to the next.
    entry_size: usize,
    /// How many headers it holds.
    count: usize,
}

impl ProgramHeaders<'_> {
    /// Each header, in order; an error for one past the table's end.
    fn headers(&self) -> impl Iterator<Item = Result<ProgramHeader, ParseError>> + '_ {
        (0..self.count).map(|index| {
            let mut offset = index * self.entry_size;
            ProgramHeader::parse_at(LittleEndian, Class::ELF64, &mut offset, self.table)
        })
    }
}

/// An image `parse` recognized.
pub struct Image<'a> {
    /// Its entry point, rebased.
    pub entry: usize,
    /// The protection it asks for its stack: the last `PT_GNU_STACK`'s, else
    /// all.
    pub stack_protection: Protection,
    /// Its program headers.
    program_headers: ProgramHeaders<'a>,
    /// What a position-independent image is rebased by; else 0.
    base: usize,
}

impl Image<'_> {
    /// Its `PT_LOAD` segments, in order.
    pub fn segments(&self) -> impl Iterator<Item = Segment> + '_ {
        self.program_headers
            .headers()
            // `parse` read each one.
            .filter_map(Result::ok)
            .filter(|header| header.p_type == abi::PT_LOAD)
            .map(|header| Segment {
                offset: usize::try_from(header.p_offset).unwrap(),
                file_size: usize::try_from(header.p_filesz).unwrap(),
                address: usize::try_from(header.p_vaddr)
                    .unwrap()
                    .wrapping_add(self.base),
                memory_size: usize::try_from(header.p_memsz).unwrap(),
                section_type: SectionType::ALLOCATE
                    | SectionType::LOAD
                    | SectionType::from_bits_truncate(protection(header.p_flags).bits()),
            })
    }
}

/// Reads the image in `bytes`: its header, then its program headers, kept
/// in place.
///
/// # Errors
///
/// `OutOfBounds` if the header or the program headers lie past the end,
/// `NotExecutable` if it is not ELF, `WrongArchitecture` if it is not 64-bit
/// little-endian `x86_64`, and `Corrupt` if a program header lies past the
/// table's end.
pub fn parse(bytes: &[u8]) -> Result<Image<'_>, Error> {
    let raw = bytes
        .get(..size_of::<Elf64_Ehdr>())
        .ok_or(Error::OutOfBounds)?;
    if !raw.starts_with(&abi::ELFMAGIC) {
        return Err(Error::NotExecutable);
    }
    if raw[abi::EI_CLASS] != abi::ELFCLASS64 || raw[abi::EI_DATA] != abi::ELFDATA2LSB {
        return Err(Error::WrongArchitecture);
    }
    let ident = (
        LittleEndian,
        Class::ELF64,
        raw[abi::EI_OSABI],
        raw[abi::EI_ABIVERSION],
    );
    let header = FileHeader::parse_tail(ident, &raw[abi::EI_NIDENT..])?;
    if header.e_machine != abi::EM_X86_64 {
        return Err(Error::WrongArchitecture);
    }
    let base = match header.e_type {
        abi::ET_DYN | abi::ET_REL => PIE_BASE,
        _ => 0,
    };
    let entry = usize::try_from(header.e_entry).unwrap().wrapping_add(base);
    let entry_size = usize::from(header.e_phentsize);
    let count = usize::from(header.e_phnum);
    let start = usize::try_from(header.e_phoff).unwrap();
    let table = start
        .checked_add(entry_size * count)
        .and_then(|end| bytes.get(start..end))
        .ok_or(Error::OutOfBounds)?;
    let program_headers = ProgramHeaders {
        table,
        entry_size,
        count,
    };
    let mut stack_protection = Protection::all();
    for program_header in program_headers.headers() {
        let program_header = program_header?;
        if program_header.p_type == abi::PT_GNU_STACK {
            stack_protection = protection(program_header.p_flags);
        }
    }
    Ok(Image {
        entry,
        stack_protection,
        program_headers,
        base,
    })
}
