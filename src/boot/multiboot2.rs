// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Multiboot2, from its specification: header values, and the boot
//! information read in place, trusting the loader.

use core::marker::PhantomData;
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
    pub const MMAP: Self = Self(6);
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

/// Start of the memory map tag; the entries follow.
#[repr(C)]
struct MmapTag {
    header: TagHeader,
    entry_size: u32,
    _entry_version: u32,
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
    Mmap(Mmap<'a>),
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
                TagType::MMAP => Self::Mmap(Mmap::new(address)),
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
            let entry = unsafe { ptr::with_exposed_provenance::<MmapEntry>(self.next).read() };
            self.next += self.entry_size;
            entry
        })
    }
}

/// One memory map entry.
#[derive(Clone, Copy)]
#[repr(C)]
pub struct MmapEntry {
    pub base_addr: u64,
    pub length: u64,
    pub ty: MemoryType,
    _reserved: u32,
}
