// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Multiboot2, from its specification: header values, and the boot
//! information read in place.

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
pub struct MemoryType(pub u32);

impl MemoryType {
    pub const AVAILABLE: Self = Self(1);
    pub const ACPI_RECLAIMABLE: Self = Self(3);
    pub const NVS: Self = Self(4);
    pub const BADRAM: Self = Self(5);
}

/// Bytes of the structure's and of each tag's header; tags are aligned to it.
const HEADER_SIZE: usize = 8;

/// Smallest memory map entry.
const MMAP_ENTRY_SIZE: usize = 24;

/// The boot information structure.
#[derive(Clone, Copy)]
pub struct Info<'a>(&'a [u8]);

impl<'a> Info<'a> {
    /// The structure at `address`.
    ///
    /// # Safety
    ///
    /// `address` holds the loader's structure, 8-byte aligned, readable and
    /// unchanged for `'a`.
    pub unsafe fn from_address(address: usize) -> Self {
        let start = ptr::with_exposed_provenance::<u32>(address);
        // SAFETY: guaranteed by the caller; `total_size` comes first.
        let total_size = usize::try_from(unsafe { start.read() }).unwrap_or(0);
        // SAFETY: guaranteed by the caller.
        Self(unsafe { slice::from_raw_parts(start.cast::<u8>(), total_size) })
    }

    /// The tags before the end tag, in order; stops at a malformed one.
    pub fn tags(self) -> impl Iterator<Item = Tag<'a>> {
        let mut rest = self.0.get(HEADER_SIZE..).unwrap_or_default();
        iter::from_fn(move || {
            let ty = TagType(read_u32(rest, 0)?);
            let size = usize::try_from(read_u32(rest, 4)?).ok()?;
            let body = rest.get(HEADER_SIZE..size)?;
            rest = rest
                .get(size.next_multiple_of(HEADER_SIZE)..)
                .unwrap_or_default();
            Some((ty, body))
        })
        .take_while(|&(ty, _)| ty != TagType::END)
        .map(|(ty, body)| Tag::parse(ty, body))
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
    /// A type not read here, or a malformed tag.
    Other,
}

impl<'a> Tag<'a> {
    /// The tag of type `ty`, whose contents after its header are `body`.
    fn parse(ty: TagType, body: &'a [u8]) -> Self {
        let tag = match ty {
            TagType::MMAP => Mmap::parse(body).map(Self::Mmap),
            TagType::ACPI_OLD => Some(Self::AcpiOld(body)),
            TagType::ACPI_NEW => Some(Self::AcpiNew(body)),
            TagType::EFI64 => read_u64(body, 0).map(Self::Efi64),
            _ => None,
        };
        tag.unwrap_or(Self::Other)
    }
}

/// The memory map's entries.
#[derive(Clone)]
pub struct Mmap<'a> {
    entry_size: usize,
    entries: &'a [u8],
}

impl<'a> Mmap<'a> {
    /// The map in a memory map tag's `body`: entry size and version, then
    /// the entries.
    fn parse(body: &'a [u8]) -> Option<Self> {
        let entry_size = usize::try_from(read_u32(body, 0)?).ok()?;
        (entry_size >= MMAP_ENTRY_SIZE).then(|| Self {
            entry_size,
            entries: body.get(8..).unwrap_or_default(),
        })
    }
}

impl Iterator for Mmap<'_> {
    type Item = MmapEntry;

    fn next(&mut self) -> Option<MmapEntry> {
        let entry = self.entries.get(..self.entry_size)?;
        self.entries = &self.entries[self.entry_size..];
        Some(MmapEntry {
            base_addr: read_u64(entry, 0)?,
            length: read_u64(entry, 8)?,
            ty: MemoryType(read_u32(entry, 16)?),
        })
    }
}

/// One memory map entry.
#[derive(Clone, Copy)]
pub struct MmapEntry {
    pub base_addr: u64,
    pub length: u64,
    pub ty: MemoryType,
}

/// The native-endian `u32` at `at`.
fn read_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let bytes = bytes.get(at..at + 4)?;
    Some(u32::from_ne_bytes(<[u8; 4]>::try_from(bytes).ok()?))
}

/// The native-endian `u64` at `at`.
fn read_u64(bytes: &[u8], at: usize) -> Option<u64> {
    let bytes = bytes.get(at..at + 8)?;
    Some(u64::from_ne_bytes(<[u8; 8]>::try_from(bytes).ok()?))
}
