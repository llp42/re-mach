// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! ACPI root system description pointer (RSDP).

use core::fmt::Write;
use core::ops::Range;
use core::{ptr, slice};

use super::serial::Serial;
use crate::boot::multiboot2::{Info, Tag};
use crate::sync::OnceLock;

/// A verified copy of the RSDP, by revision.
#[expect(dead_code, reason = "the ACPI tables are found through it")]
enum Rsdp {
    /// ACPI 1.0's.
    V1([u8; Self::V1_SIZE]),
    /// ACPI >= 2.0's.
    V2([u8; Self::V2_SIZE]),
}

impl Rsdp {
    const SIGNATURE: &[u8; 8] = b"RSD PTR ";
    /// Its alignment in BIOS memory.
    const ALIGN: usize = 16;
    /// Offset of its revision byte.
    const REVISION: usize = 15;
    const V1_SIZE: usize = 20;
    const V2_SIZE: usize = 36;

    /// A copy of the RSDP at the start of `bytes`, if its signature,
    /// revision and checksum are good.
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..Self::SIGNATURE.len())? != Self::SIGNATURE {
            return None;
        }
        let size = match *bytes.get(Self::REVISION)? {
            0 => {
                let _ = write!(Serial, "ACPI v1.0\r\n");
                Self::V1_SIZE
            }
            2 => {
                let _ = write!(Serial, "ACPI >= v2.0\r\n");
                Self::V2_SIZE
            }
            _ => return None,
        };
        let rsdp = bytes.get(..size)?;
        if rsdp.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)) != 0 {
            return None;
        }
        Some(if size == Self::V1_SIZE {
            Self::V1(rsdp.try_into().ok()?)
        } else {
            Self::V2(rsdp.try_into().ok()?)
        })
    }

    /// Searches the EBDA's first KiB, then the BIOS area at 0xe0000.
    ///
    /// # Safety
    ///
    /// The first 2 MiB are identity-mapped and readable.
    unsafe fn find_in_bios() -> Option<Self> {
        // SAFETY: the BIOS data area's EBDA segment; guaranteed by the
        // caller.
        let segment = unsafe { ptr::with_exposed_provenance::<u16>(0x40e).read() };
        let ebda = usize::from(segment) << 4;
        // SAFETY: guaranteed by the caller.
        unsafe { Self::find(ebda..ebda + 1024).or_else(|| Self::find(0xe_0000..0x10_0000)) }
    }

    /// The first RSDP in `range`, on 16-byte boundaries.
    ///
    /// # Safety
    ///
    /// `range`, and 36 bytes past it, are identity-mapped and readable.
    unsafe fn find(range: Range<usize>) -> Option<Self> {
        range
            .step_by(Self::ALIGN)
            .filter(|&address| address != 0)
            .find_map(|address| {
                let start = ptr::with_exposed_provenance::<u8>(address);
                // SAFETY: guaranteed by the caller.
                Self::parse(unsafe { slice::from_raw_parts(start, Self::V2_SIZE) })
            })
    }
}

/// Copy of the verified RSDP, once found.
static RSDP: OnceLock<Rsdp> = OnceLock::new();

/// The EFI system table's physical address, when booted through UEFI.
pub static EFI_SYSTEM_TABLE: OnceLock<u64> = OnceLock::new();

/// Saves the EFI system table address and a copy of the verified RSDP:
/// the boot information's newest, or else the BIOS's.
///
/// # Safety
///
/// Runs once, on the boot CPU. The first 2 MiB are identity-mapped and
/// readable.
pub unsafe fn init(boot_info: Info) {
    let (mut old, mut new) = (None, None);
    for tag in boot_info.tags() {
        match tag {
            Tag::AcpiOld(copy) => old = Some(copy),
            Tag::AcpiNew(copy) => new = Some(copy),
            Tag::Efi64(address) => {
                assert!(
                    EFI_SYSTEM_TABLE.set(address).is_ok(),
                    "acpi: two EFI system tables"
                );
            }
            _ => {}
        }
    }
    let rsdp = new
        .or(old)
        .and_then(Rsdp::parse)
        // SAFETY: guaranteed by the caller.
        .or_else(|| unsafe { Rsdp::find_in_bios() });
    if let Some(rsdp) = rsdp {
        assert!(RSDP.set(rsdp).is_ok(), "acpi: RSDP saved twice");
    }
}
