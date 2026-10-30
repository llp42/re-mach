// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! The running image, a PE32+ file.

use core::{ptr, slice};

use super::{AllocateType, MemoryType, PhysicalAddress, Status, SystemTable};

unsafe extern "C" {
    /// The image's first byte, its DOS header.
    safe static __ImageBase: u8;
}

/// `EFI_PAGE_SIZE`.
const PAGE_SIZE: usize = 4096;

/// `IMAGE_REL_BASED_ABSOLUTE`: padding.
const REL_BASED_ABSOLUTE: u16 = 0;

/// `IMAGE_REL_BASED_DIR64`: a 64-bit address to shift.
const REL_BASED_DIR64: u16 = 10;

/// Moves the running image below `max_address`, relocated for its new
/// place; returns how far it moved. The old copy stays intact.
///
/// Errors: the firmware's status if no room is free there, `LOAD_ERROR` for
/// bad headers, `UNSUPPORTED` for an unknown relocation type.
///
/// # Safety
///
/// Boot services active; nothing in the image changed since it was loaded.
pub unsafe fn move_image(
    system_table: &SystemTable,
    max_address: PhysicalAddress,
) -> Result<usize, Status> {
    let base = (&raw const __ImageBase).addr();
    // SAFETY: the loader keeps the PE headers: `e_lfanew` at 0x3c, then the
    // PE32+ `SizeOfImage` 80 bytes past it.
    let size = unsafe {
        let e_lfanew = ptr::with_exposed_provenance::<u32>(base + 0x3c).read_unaligned();
        let e_lfanew = usize::try_from(e_lfanew).map_err(|_| Status::LOAD_ERROR)?;
        ptr::with_exposed_provenance::<u32>(base + e_lfanew + 80).read_unaligned()
    };
    let size = usize::try_from(size).map_err(|_| Status::LOAD_ERROR)?;
    // SAFETY: guaranteed by the caller.
    let address = unsafe {
        system_table.allocate_pages(
            AllocateType::MaxAddress,
            MemoryType::LOADER_CODE,
            size.div_ceil(PAGE_SIZE),
            max_address,
        )?
    };
    let new = usize::try_from(address).map_err(|_| Status::LOAD_ERROR)?;
    // SAFETY: the loaded image spans `SizeOfImage` bytes from its base.
    let old = unsafe { slice::from_raw_parts(ptr::with_exposed_provenance::<u8>(base), size) };
    // SAFETY: just allocated, at least `size` bytes; apart from `old`.
    let copy =
        unsafe { slice::from_raw_parts_mut(ptr::with_exposed_provenance_mut::<u8>(new), size) };
    copy.copy_from_slice(old);
    let shift = new.wrapping_sub(base);
    relocate(copy, u64::try_from(shift).map_err(|_| Status::LOAD_ERROR)?)?;
    Ok(shift)
}

/// Adds `shift` to every address the base relocations of `image` list.
fn relocate(image: &mut [u8], shift: u64) -> Result<(), Status> {
    let e_lfanew = usize::try_from(read_u32(image, 0x3c)?).map_err(|_| Status::LOAD_ERROR)?;
    // PE32+ optional header: `NumberOfRvaAndSizes`, then the data
    // directories; the base relocation table is the sixth.
    let optional = e_lfanew + 24;
    if read_u32(image, optional + 108)? <= 5 {
        return Ok(());
    }
    let mut block =
        usize::try_from(read_u32(image, optional + 152)?).map_err(|_| Status::LOAD_ERROR)?;
    let end = block
        + usize::try_from(read_u32(image, optional + 156)?).map_err(|_| Status::LOAD_ERROR)?;
    while block < end {
        let page = usize::try_from(read_u32(image, block)?).map_err(|_| Status::LOAD_ERROR)?;
        let block_size =
            usize::try_from(read_u32(image, block + 4)?).map_err(|_| Status::LOAD_ERROR)?;
        if block_size < 8 {
            return Err(Status::LOAD_ERROR);
        }
        for entry in (block + 8..block + block_size).step_by(2) {
            let entry = read_u16(image, entry)?;
            let at = page + usize::from(entry & 0xfff);
            match entry >> 12 {
                REL_BASED_ABSOLUTE => {}
                REL_BASED_DIR64 => {
                    let target = image.get_mut(at..at + 8).ok_or(Status::LOAD_ERROR)?;
                    let value = u64::from_le_bytes(
                        <[u8; 8]>::try_from(&*target).map_err(|_| Status::LOAD_ERROR)?,
                    );
                    target.copy_from_slice(&value.wrapping_add(shift).to_le_bytes());
                }
                _ => return Err(Status::UNSUPPORTED),
            }
        }
        block += block_size;
    }
    Ok(())
}

/// The little-endian `u16` at `at`.
fn read_u16(image: &[u8], at: usize) -> Result<u16, Status> {
    let bytes = image.get(at..at + 2).ok_or(Status::LOAD_ERROR)?;
    Ok(u16::from_le_bytes(
        <[u8; 2]>::try_from(bytes).map_err(|_| Status::LOAD_ERROR)?,
    ))
}

/// The little-endian `u32` at `at`.
fn read_u32(image: &[u8], at: usize) -> Result<u32, Status> {
    let bytes = image.get(at..at + 4).ok_or(Status::LOAD_ERROR)?;
    Ok(u32::from_le_bytes(
        <[u8; 4]>::try_from(bytes).map_err(|_| Status::LOAD_ERROR)?,
    ))
}
