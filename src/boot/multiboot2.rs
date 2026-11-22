// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Multiboot2, from its specification: header values, and the boot
//! information read in place, trusting the loader.

use core::ffi::CStr;
use core::marker::PhantomData;
use core::ops::Range;
use core::{iter, ptr, slice};

/// Header magic.
pub const HEADER_MAGIC: u32 = 0xe852_50d6;

/// Header architecture: 32-bit protected mode x86.
pub const ARCHITECTURE_X86: u32 = 0;

/// `%eax` at entry from a Multiboot2 loader.
pub const BOOTLOADER_MAGIC: u32 = 0x36d7_6289;

/// Header tag type.
pub struct HeaderTagType(pub u16);

impl HeaderTagType {
    pub const END: Self = Self(0);
    pub const INFORMATION_REQUEST: Self = Self(1);
}

/// Information tag type; loaders may add types the kernel does not know.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct TagType(pub u32);

impl TagType {
    pub const END: Self = Self(0);
    pub const CMDLINE: Self = Self(1);
    pub const MODULE: Self = Self(3);
    pub const MMAP: Self = Self(6);
    pub const ELF_SECTIONS: Self = Self(9);
    pub const EFI64: Self = Self(12);
    pub const ACPI_OLD: Self = Self(14);
    pub const ACPI_NEW: Self = Self(15);
}

/// Memory map entry type; any other value is reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct MemoryType(pub u32);

impl MemoryType {
    pub const AVAILABLE: Self = Self(1);
    pub const ACPI_RECLAIMABLE: Self = Self(3);
    pub const NVS: Self = Self(4);
    pub const BADRAM: Self = Self(5);
}

/// ELF section type; only those read here.
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct ElfSectionType(pub u32);

impl ElfSectionType {
    pub const SYMTAB: Self = Self(2);
    pub const STRTAB: Self = Self(3);
}

/// Bytes before the first tag: `total_size` and `reserved`.
const INFO_HEADER_SIZE: usize = 8;

/// Bytes of every tag's `type` and `size`.
const TAG_HEADER_SIZE: usize = 8;

/// Tags start on 8-byte boundaries.
const TAG_ALIGN: usize = 8;

/// The boot information structure.
#[derive(Clone, Copy)]
pub struct Info<'a> {
    address: usize,
    _info: PhantomData<&'a [u8]>,
}

impl<'a> Info<'a> {
    /// The structure at `address`.
    ///
    /// # Safety
    ///
    /// `address` holds the loader's structure, readable and unchanged for `'a`.
    pub const unsafe fn from_address(address: usize) -> Self {
        Self {
            address,
            _info: PhantomData,
        }
    }

    /// The structure's physical range.
    pub fn range(self) -> Range<usize> {
        self.address..self.address + self.bytes().len()
    }

    /// The tags before the end tag, in order.
    pub fn tags(self) -> impl Iterator<Item = Tag<'a>> {
        let mut rest = &self.bytes()[INFO_HEADER_SIZE..];
        iter::from_fn(move || {
            let ty = TagType(u32::from_le_bytes(*rest.first_chunk().unwrap()));
            let size =
                usize::try_from(u32::from_le_bytes(*rest[4..].first_chunk().unwrap())).unwrap();
            let tag = &rest[..size];
            rest = &rest[size.next_multiple_of(TAG_ALIGN)..];
            Some((ty, tag))
        })
        .take_while(|&(ty, _)| ty != TagType::END)
        .map(|(ty, tag)| Tag::parse(ty, &tag[TAG_HEADER_SIZE..]))
    }

    /// The memory map, if a tag holds one.
    pub fn mmap(self) -> Option<Mmap<'a>> {
        self.tags().find_map(|tag| match tag {
            Tag::Mmap(mmap) => Some(mmap),
            _ => None,
        })
    }

    /// The structure's bytes, `total_size` of them.
    fn bytes(self) -> &'a [u8] {
        // SAFETY: the structure, `total_size` first, is readable and
        // unchanged for `'a`; guaranteed by `from_address`.
        unsafe {
            let total_size = ptr::with_exposed_provenance::<u32>(self.address).read();
            slice::from_raw_parts(
                ptr::with_exposed_provenance(self.address),
                usize::try_from(total_size).unwrap(),
            )
        }
    }
}

/// An information tag.
pub enum Tag<'a> {
    /// The kernel's command line.
    Cmdline(&'a CStr),
    /// A module's physical range, `mod_end` past its last byte, and its
    /// command line.
    Module {
        mod_start: usize,
        mod_end: usize,
        cmdline: &'a CStr,
    },
    Mmap(Mmap<'a>),
    /// The kernel's ELF section headers.
    ElfSections(ElfSections<'a>),
    /// Copy of the ACPI 1.0 RSDP.
    AcpiOld(&'a [u8]),
    /// Copy of the ACPI >= 2.0 RSDP.
    AcpiNew(&'a [u8]),
    /// Physical address of the EFI system table.
    Efi64(u64),
    /// A type not read here.
    Other,
}

impl<'a> Tag<'a> {
    /// The tag of type `ty` whose bytes past its header are `body`.
    fn parse(ty: TagType, body: &'a [u8]) -> Self {
        match ty {
            TagType::CMDLINE => Self::Cmdline(CStr::from_bytes_until_nul(body).unwrap()),
            TagType::MODULE => Self::Module {
                mod_start: usize::try_from(u32::from_le_bytes(*body.first_chunk().unwrap()))
                    .unwrap(),
                mod_end: usize::try_from(u32::from_le_bytes(*body[4..].first_chunk().unwrap()))
                    .unwrap(),
                cmdline: CStr::from_bytes_until_nul(&body[8..]).unwrap(),
            },
            TagType::MMAP => Self::Mmap(Mmap::new(body)),
            TagType::ELF_SECTIONS => Self::ElfSections(ElfSections::new(body)),
            TagType::ACPI_OLD => Self::AcpiOld(body),
            TagType::ACPI_NEW => Self::AcpiNew(body),
            TagType::EFI64 => Self::Efi64(u64::from_le_bytes(*body.first_chunk().unwrap())),
            _ => Self::Other,
        }
    }
}

/// The memory map's entries.
#[derive(Clone)]
pub struct Mmap<'a> {
    entries: &'a [u8],
    entry_size: usize,
}

impl<'a> Mmap<'a> {
    /// The map in the memory map tag whose bytes past its header are `body`.
    fn new(body: &'a [u8]) -> Self {
        Self {
            entry_size: usize::try_from(u32::from_le_bytes(*body.first_chunk().unwrap())).unwrap(),
            // Past `entry_size` and `entry_version`.
            entries: &body[8..],
        }
    }
}

impl Iterator for Mmap<'_> {
    type Item = MmapEntry;

    fn next(&mut self) -> Option<MmapEntry> {
        (!self.entries.is_empty()).then(|| {
            let (entry, rest) = self.entries.split_at(self.entry_size);
            self.entries = rest;
            MmapEntry {
                base_addr: usize::try_from(u64::from_le_bytes(*entry.first_chunk().unwrap()))
                    .unwrap(),
                length: usize::try_from(u64::from_le_bytes(*entry[8..].first_chunk().unwrap()))
                    .unwrap(),
                ty: MemoryType(u32::from_le_bytes(*entry[16..].first_chunk().unwrap())),
            }
        })
    }
}

/// One memory map entry.
#[derive(Clone, Copy)]
pub struct MmapEntry {
    pub base_addr: usize,
    pub length: usize,
    pub ty: MemoryType,
}

/// The ELF sections tag's section headers.
#[derive(Clone)]
pub struct ElfSections<'a> {
    headers: &'a [u8],
    entsize: usize,
    left: usize,
}

impl<'a> ElfSections<'a> {
    /// The section headers in the ELF sections tag whose bytes past its
    /// header are `body`.
    fn new(body: &'a [u8]) -> Self {
        Self {
            left: usize::try_from(u32::from_le_bytes(*body.first_chunk().unwrap())).unwrap(),
            entsize: usize::try_from(u32::from_le_bytes(*body[4..].first_chunk().unwrap()))
                .unwrap(),
            // Past `num`, `entsize` and `shndx`.
            headers: &body[12..],
        }
    }
}

impl Iterator for ElfSections<'_> {
    type Item = ElfSection;

    fn next(&mut self) -> Option<ElfSection> {
        (self.left > 0).then(|| {
            let (section, rest) = self.headers.split_at(self.entsize);
            self.headers = rest;
            self.left -= 1;
            ElfSection {
                ty: ElfSectionType(u32::from_le_bytes(*section[4..].first_chunk().unwrap())),
                addr: usize::try_from(u64::from_le_bytes(*section[16..].first_chunk().unwrap()))
                    .unwrap(),
                size: usize::try_from(u64::from_le_bytes(*section[32..].first_chunk().unwrap()))
                    .unwrap(),
            }
        })
    }
}

/// One ELF section header.
#[derive(Clone, Copy)]
pub struct ElfSection {
    pub ty: ElfSectionType,
    pub addr: usize,
    pub size: usize,
}
