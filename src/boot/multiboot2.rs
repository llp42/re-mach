// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Multiboot2, from its specification: header values, and the boot
//! information read in place, trusting the loader.

use core::ffi::{CStr, c_char};
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

/// Tags start on 8-byte boundaries.
const TAG_ALIGN: usize = 8;

/// Start of every tag.
#[repr(C)]
struct TagHeader {
    ty: TagType,
    size: u32,
}

/// Start of the module tag; the module's command line follows.
#[repr(C)]
struct ModuleTag {
    _header: TagHeader,
    mod_start: u32,
    mod_end: u32,
}

/// Start of the memory map tag; the entries follow.
#[repr(C)]
struct MmapTag {
    header: TagHeader,
    entry_size: u32,
    _entry_version: u32,
}

/// Start of the ELF sections tag; the section headers follow, 4-byte
/// aligned.
#[repr(C)]
struct ElfSectionsTag {
    _header: TagHeader,
    num: u32,
    entsize: u32,
    _shndx: u32,
}

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
        // SAFETY: `total_size` comes first; guaranteed by `from_address`.
        let total_size = unsafe { ptr::with_exposed_provenance::<u32>(self.address).read() };
        self.address..self.address + usize::try_from(total_size).unwrap()
    }

    /// The tags before the end tag, in order.
    pub fn tags(self) -> impl Iterator<Item = Tag<'a>> {
        let mut address = self.address + INFO_HEADER_SIZE;
        iter::from_fn(move || {
            let tag = address;
            // SAFETY: the loader puts its tags one after another, the end
            // tag last.
            let header = unsafe { ptr::with_exposed_provenance::<TagHeader>(tag).read() };
            address += usize::try_from(header.size)
                .unwrap()
                .next_multiple_of(TAG_ALIGN);
            Some((tag, header))
        })
        .take_while(|(_, header)| header.ty != TagType::END)
        // SAFETY: as above.
        .map(|(tag, header)| unsafe { Tag::parse(tag, &header) })
    }

    /// The memory map, if a tag holds one.
    pub fn mmap(self) -> Option<Mmap<'a>> {
        self.tags().find_map(|tag| match tag {
            Tag::Mmap(mmap) => Some(mmap),
            _ => None,
        })
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

impl Tag<'_> {
    /// The tag at `address`, whose header is `header`.
    ///
    /// # Safety
    ///
    /// `address` holds the loader's tag, readable and unchanged for the
    /// result's lifetime.
    unsafe fn parse(address: usize, header: &TagHeader) -> Self {
        let body = address + size_of::<TagHeader>();
        let len = usize::try_from(header.size).unwrap() - size_of::<TagHeader>();
        // SAFETY: guaranteed by the caller.
        unsafe {
            match header.ty {
                TagType::CMDLINE => {
                    Self::Cmdline(CStr::from_ptr(ptr::with_exposed_provenance::<c_char>(body)))
                }
                TagType::MODULE => {
                    let tag = ptr::with_exposed_provenance::<ModuleTag>(address).read();
                    let cmdline = address + size_of::<ModuleTag>();
                    Self::Module {
                        mod_start: usize::try_from(tag.mod_start).unwrap(),
                        mod_end: usize::try_from(tag.mod_end).unwrap(),
                        cmdline: CStr::from_ptr(ptr::with_exposed_provenance::<c_char>(cmdline)),
                    }
                }
                TagType::MMAP => Self::Mmap(Mmap::new(address)),
                TagType::ELF_SECTIONS => Self::ElfSections(ElfSections::new(address)),
                TagType::ACPI_OLD => Self::AcpiOld(slice::from_raw_parts(
                    ptr::with_exposed_provenance(body),
                    len,
                )),
                TagType::ACPI_NEW => Self::AcpiNew(slice::from_raw_parts(
                    ptr::with_exposed_provenance(body),
                    len,
                )),
                TagType::EFI64 => Self::Efi64(ptr::with_exposed_provenance::<u64>(body).read()),
                _ => Self::Other,
            }
        }
    }
}

/// The memory map's entries.
#[derive(Clone)]
pub struct Mmap<'a> {
    next: usize,
    end: usize,
    entry_size: usize,
    _info: PhantomData<&'a [u8]>,
}

impl Mmap<'_> {
    /// The map in the memory map tag at `address`.
    ///
    /// # Safety
    ///
    /// `address` holds the loader's memory map tag, readable and unchanged
    /// for the result's lifetime.
    unsafe fn new(address: usize) -> Self {
        // SAFETY: guaranteed by the caller.
        let tag = unsafe { ptr::with_exposed_provenance::<MmapTag>(address).read() };
        Self {
            next: address + size_of::<MmapTag>(),
            end: address + usize::try_from(tag.header.size).unwrap(),
            entry_size: usize::try_from(tag.entry_size).unwrap(),
            _info: PhantomData,
        }
    }
}

impl Iterator for Mmap<'_> {
    type Item = MmapEntry;

    fn next(&mut self) -> Option<MmapEntry> {
        (self.next < self.end).then(|| {
            // SAFETY: the loader's entries fill the tag, `entry_size` apart.
            let entry = unsafe { ptr::with_exposed_provenance::<RawMmapEntry>(self.next).read() };
            self.next += self.entry_size;
            MmapEntry {
                base_addr: usize::try_from(entry.base_addr).unwrap(),
                length: usize::try_from(entry.length).unwrap(),
                ty: entry.ty,
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

/// A memory map entry as the loader lays it out.
#[repr(C)]
struct RawMmapEntry {
    base_addr: u64,
    length: u64,
    ty: MemoryType,
    _reserved: u32,
}

/// The ELF sections tag's section headers.
#[derive(Clone)]
pub struct ElfSections<'a> {
    next: usize,
    left: usize,
    entsize: usize,
    _info: PhantomData<&'a [u8]>,
}

impl ElfSections<'_> {
    /// The section headers in the ELF sections tag at `address`.
    ///
    /// # Safety
    ///
    /// `address` holds the loader's ELF sections tag, readable and unchanged
    /// for the result's lifetime.
    unsafe fn new(address: usize) -> Self {
        // SAFETY: guaranteed by the caller.
        let tag = unsafe { ptr::with_exposed_provenance::<ElfSectionsTag>(address).read() };
        Self {
            next: address + size_of::<ElfSectionsTag>(),
            left: usize::try_from(tag.num).unwrap(),
            entsize: usize::try_from(tag.entsize).unwrap(),
            _info: PhantomData,
        }
    }
}

impl Iterator for ElfSections<'_> {
    type Item = ElfSection;

    fn next(&mut self) -> Option<ElfSection> {
        (self.left > 0).then(|| {
            // SAFETY: the loader's `num` headers follow the tag, `entsize`
            // apart.
            let section = unsafe {
                ptr::with_exposed_provenance::<RawElfSection>(self.next).read_unaligned()
            };
            self.next += self.entsize;
            self.left -= 1;
            ElfSection {
                ty: section.ty,
                addr: usize::try_from(section.addr).unwrap(),
                size: usize::try_from(section.size).unwrap(),
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

/// An ELF64 section header as the loader lays it out.
#[repr(C)]
struct RawElfSection {
    _name: u32,
    ty: ElfSectionType,
    _flags: u64,
    addr: u64,
    _offset: u64,
    size: u64,
    _link: u32,
    _info: u32,
    _addralign: u64,
    _entsize: u64,
}
