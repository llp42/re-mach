// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Loaded PE32+ images (Microsoft PE format): headers, sections and base
//! relocations.
//!
//! Spec names in Rust case, without the `IMAGE_` prefix.

/// `IMAGE_SCN_MEM_EXECUTE`.
pub const SCN_MEM_EXECUTE: u32 = 0x2000_0000;
/// `IMAGE_SCN_MEM_WRITE`.
pub const SCN_MEM_WRITE: u32 = 0x8000_0000;

/// `IMAGE_REL_BASED_DIR64`: add the delta to a 64-bit field.
const REL_BASED_DIR64: u16 = 10;

/// Offset of `e_lfanew`, the PE signature's offset, in the DOS header.
const DOS_LFANEW: usize = 0x3c;
/// The COFF header follows the 4-byte signature; the optional header, its 20 bytes.
const COFF_HEADER: usize = 4;
const OPTIONAL_HEADER: usize = 24;
/// Offsets in the COFF header.
const NUMBER_OF_SECTIONS: usize = 2;
const SIZE_OF_OPTIONAL_HEADER: usize = 16;
/// Offsets in the PE32+ optional header.
const SIZE_OF_HEADERS: usize = 60;
/// The base relocation table's data directory (entry 5).
const BASE_RELOCATION_TABLE: usize = 152;
/// Section header size, and offsets in it.
const SECTION_HEADER_SIZE: usize = 40;
const VIRTUAL_SIZE: usize = 8;
const VIRTUAL_ADDRESS: usize = 12;
const CHARACTERISTICS: usize = 36;

unsafe extern "C" {
    /// The linker's symbol for this image's first byte.
    #[link_name = "__ImageBase"]
    static IMAGE_BASE: u8;
}

/// A section's place and access, from its header.
pub struct Section {
    pub virtual_address: u32,
    pub virtual_size: u32,
    pub characteristics: u32,
}

/// A PE32+ image loaded in memory.
#[derive(Clone, Copy)]
pub struct Image(*const u8);

impl Image {
    /// This kernel's own image.
    pub fn this() -> Self {
        Self(&raw const IMAGE_BASE)
    }

    /// The image's current address.
    pub fn address(self) -> u64 {
        u64::try_from(self.0.expose_provenance()).unwrap()
    }

    pub fn size_of_headers(self) -> u32 {
        self.read_u32(self.optional_header() + SIZE_OF_HEADERS)
    }

    pub fn sections(self) -> impl Iterator<Item = Section> {
        let coff = self.pe_header() + COFF_HEADER;
        let count = self.read_u16(coff + NUMBER_OF_SECTIONS);
        let table =
            self.optional_header() + usize::from(self.read_u16(coff + SIZE_OF_OPTIONAL_HEADER));
        (0..usize::from(count)).map(move |index| {
            let header = table + index * SECTION_HEADER_SIZE;
            Section {
                virtual_address: self.read_u32(header + VIRTUAL_ADDRESS),
                virtual_size: self.read_u32(header + VIRTUAL_SIZE),
                characteristics: self.read_u32(header + CHARACTERISTICS),
            }
        })
    }

    /// Adds `delta` to every `REL_BASED_DIR64` relocation; skips padding and
    /// other types.
    ///
    /// # Safety
    ///
    /// The image is writable, and every pointer it holds stays valid both
    /// before and after `delta` is added.
    pub unsafe fn relocate(self, delta: u64) {
        let directory = self.optional_header() + BASE_RELOCATION_TABLE;
        let table = self.offset(directory);
        let end = table + self.offset(directory + 4);
        let mut block = table;
        while block + 8 <= end {
            let page = self.offset(block);
            let size = self.offset(block + 4);
            if size < 8 {
                break;
            }
            for entry in (block + 8..block + size).step_by(2) {
                let entry = self.read_u16(entry);
                if entry >> 12 == REL_BASED_DIR64 {
                    // SAFETY: the relocation names a field of this image.
                    unsafe {
                        let field = self.0.add(page + usize::from(entry & 0xfff)).cast_mut();
                        let value = field.cast::<u64>().read_unaligned().wrapping_add(delta);
                        field.cast::<u64>().write_unaligned(value);
                    }
                }
            }
            block += size;
        }
    }

    fn pe_header(self) -> usize {
        self.offset(DOS_LFANEW)
    }

    fn optional_header(self) -> usize {
        self.pe_header() + OPTIONAL_HEADER
    }

    /// A 32-bit field read as an offset.
    fn offset(self, at: usize) -> usize {
        usize::try_from(self.read_u32(at)).unwrap()
    }

    const fn read_u16(self, at: usize) -> u16 {
        // SAFETY: `at` lies in the image's headers or relocation table.
        unsafe { self.0.add(at).cast::<u16>().read_unaligned() }
    }

    const fn read_u32(self, at: usize) -> u32 {
        // SAFETY: `at` lies in the image's headers or relocation table.
        unsafe { self.0.add(at).cast::<u32>().read_unaligned() }
    }
}
