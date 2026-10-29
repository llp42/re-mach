// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! ACPI root system description pointer (RSDP).

use core::cell::UnsafeCell;
use core::fmt::Write;
use core::sync::atomic::Ordering;
use core::{ptr, slice};

use super::serial::Serial;
use crate::boot::BootInfo;
use crate::kern::host::HOST_EFI_SYSTAB_ADDRESS;

const RSDP_SIGNATURE: &[u8; 8] = b"RSD PTR ";
const RSDP_ALIGN: usize = 16;
/// Offset of the revision byte.
const RSDP_REVISION: usize = 15;
/// ACPI 1.0 RSDP size.
const RSDP_V1_SIZE: usize = 20;
/// ACPI >= 2.0 RSDP size.
const RSDP_V2_SIZE: usize = 36;

struct Rsdp(UnsafeCell<Option<[u8; RSDP_V2_SIZE]>>);

// SAFETY: written once by `init`, before any reader.
unsafe impl Sync for Rsdp {}

/// Copy of the verified RSDP; `None` if none was found.
static RSDP: Rsdp = Rsdp(UnsafeCell::new(None));

/// Saves the EFI system table address and a copy of the verified RSDP.
///
/// # Safety
///
/// Runs once, on the boot CPU. The first 2 MiB are identity-mapped and
/// readable; so is `boot_info`'s RSDP, for 20 bytes or 36 if revision 2.
pub unsafe fn init(boot_info: &impl BootInfo) {
    if let Some(address) = boot_info.efi_system_table() {
        HOST_EFI_SYSTAB_ADDRESS.store(address, Ordering::Relaxed);
    }
    let rsdp = boot_info
        .acpi_rsdp()
        .and_then(|address| usize::try_from(address).ok())
        .filter(|&address| address != 0)
        // SAFETY: guaranteed by the caller.
        .and_then(|address| unsafe { check(address) })
        // SAFETY: guaranteed by the caller.
        .or_else(|| unsafe { from_bios() });
    // SAFETY: no reader yet.
    unsafe { *RSDP.0.get() = rsdp };
}

/// A copy of the RSDP at `address`, if its signature, revision and
/// checksum are good.
///
/// # Safety
///
/// `address` is non-null, identity-mapped and readable for 20 bytes, or 36
/// if its revision is 2.
unsafe fn check(address: usize) -> Option<[u8; RSDP_V2_SIZE]> {
    let start = ptr::with_exposed_provenance::<u8>(address);
    // SAFETY: guaranteed by the caller.
    let v1 = unsafe { slice::from_raw_parts(start, RSDP_V1_SIZE) };
    if v1[..RSDP_SIGNATURE.len()] != *RSDP_SIGNATURE {
        return None;
    }
    let size = match v1[RSDP_REVISION] {
        0 => {
            let _ = write!(Serial, "ACPI v1.0\r\n");
            RSDP_V1_SIZE
        }
        2 => {
            let _ = write!(Serial, "ACPI >= v2.0\r\n");
            RSDP_V2_SIZE
        }
        _ => return None,
    };
    // SAFETY: guaranteed by the caller.
    let rsdp = unsafe { slice::from_raw_parts(start, size) };
    if rsdp.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)) != 0 {
        return None;
    }
    let mut copy = [0; RSDP_V2_SIZE];
    copy[..size].copy_from_slice(rsdp);
    Some(copy)
}

/// Searches the EBDA's first KiB, then the BIOS area at 0xe0000.
///
/// # Safety
///
/// The first 2 MiB are identity-mapped and readable.
unsafe fn from_bios() -> Option<[u8; RSDP_V2_SIZE]> {
    // SAFETY: the BIOS data area's EBDA segment; guaranteed by the caller.
    let segment = unsafe { ptr::with_exposed_provenance::<u16>(0x40e).read() };
    let ebda = usize::from(segment) << 4;
    // SAFETY: guaranteed by the caller.
    unsafe { search(ebda, ebda + 1024).or_else(|| search(0xe_0000, 0x10_0000)) }
}

/// The first RSDP in `start..end`, on 16-byte boundaries.
///
/// # Safety
///
/// `start..end + 36` is identity-mapped and readable.
unsafe fn search(start: usize, end: usize) -> Option<[u8; RSDP_V2_SIZE]> {
    (start..end)
        .step_by(RSDP_ALIGN)
        .filter(|&address| address != 0)
        // SAFETY: guaranteed by the caller.
        .find_map(|address| unsafe { check(address) })
}
