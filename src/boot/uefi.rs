// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from NetBSD
// Upstream-Path: sys/arch/i386/stand/efiboot/eficons.c
// Upstream-Commit: cc2030c15de6861749296c2498758dfdfc53eac5
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 2016 Kimihiro Nonaka <nonaka@netbsd.org>
//! UEFI resource collection and translation into the kernel handoff.

use uefi::boot::{self, AllocateType, MemoryAttribute, MemoryDescriptor, MemoryType};
use uefi::mem::memory_map::MemoryMap;
use uefi::proto::console::gop::{GraphicsOutput, PixelFormat};
use uefi::proto::loaded_image::LoadedImage;

use super::info::{Builder, Framebuffer, MemoryKind, MemoryRegion};
use crate::diagnostics::{self, dbg_println};

const BLOB_CAPACITY: usize = 64 * 1024;

/// Retains loader allocations and the firmware stack for the entire kernel call.
/// A future allocator must not reclaim them until their users have moved away.
pub fn enter() -> ! {
    diagnostics::print(format_args!("re-mach: UEFI boot\r\n"));
    let _console_available = uefi::system::with_stdout(|out| {
        out.output_string(uefi::cstr16!("re-mach: UEFI boot\r\n"))
            .is_ok()
    });
    dbg_println!("firmware console available: {}", _console_available);

    let image = {
        let image = require(
            boot::open_protocol_exclusive::<LoadedImage>(boot::image_handle()),
            "loaded image",
        );
        let (base, length) = image.info();
        let start = base as u64;
        let end = start
            .checked_add(length)
            .unwrap_or_else(|| diagnostics::fatal(format_args!("loaded image: address overflow")));
        start..end
    };
    let framebuffer = find_framebuffer();
    dbg_println!(
        "image: {:#x}..{:#x}; framebuffer available: {}",
        image.start,
        image.end,
        framebuffer.is_some()
    );

    // Firmware page size is a protocol unit, independent of CPU paging choices.
    let page_count = BLOB_CAPACITY.div_ceil(uefi::boot::PAGE_SIZE);
    let allocation = require(
        boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, page_count),
        "boot information allocation",
    );
    debug_assert!(
        allocation
            .as_ptr()
            .addr()
            .is_multiple_of(uefi::boot::PAGE_SIZE)
    );
    // SAFETY: the successful allocation exclusively owns at least BLOB_CAPACITY
    // writable bytes. Initialize them before forming a slice. They are never
    // freed here and remain reserved in the final memory inventory.
    let storage = unsafe {
        allocation.as_ptr().write_bytes(0, BLOB_CAPACITY);
        core::slice::from_raw_parts_mut(allocation.as_ptr(), BLOB_CAPACITY)
    };
    let mut builder = Builder::new(storage);
    require(builder.image(image), "image record");
    if let Some(framebuffer) = framebuffer {
        require(builder.framebuffer(framebuffer), "framebuffer record");
    }

    // SAFETY: all protocol guards and console references have been dropped.
    // The only retained firmware allocation is our owned loader buffer, which
    // does not invoke Boot Services on drop. The returned map owns its storage.
    let memory_map = unsafe { boot::exit_boot_services(None) };
    dbg_println!("firmware boot services exited");
    if memory_map.meta().desc_version != MemoryDescriptor::VERSION {
        diagnostics::fatal(format_args!("memory map: unsupported descriptor version"));
    }
    dbg_println!(
        "memory map: {} descriptors, stride={}",
        memory_map.len(),
        memory_map.meta().desc_size
    );
    for descriptor in memory_map.entries() {
        let byte_len = descriptor
            .page_count
            .checked_mul(uefi::boot::PAGE_SIZE as u64)
            .unwrap_or_else(|| diagnostics::fatal(format_args!("memory map: size overflow")));
        // Loader, boot-services, runtime, MMIO and unknown types remain reserved.
        // In particular, the image, blob, memory map and current stack stay live.
        let kind = if descriptor.ty == MemoryType::CONVENTIONAL
            && !descriptor.att.contains(MemoryAttribute::RUNTIME)
        {
            MemoryKind::Available
        } else {
            MemoryKind::Reserved
        };
        require(
            builder.memory_region(MemoryRegion {
                physical_start: descriptor.phys_start,
                byte_len,
                kind,
            }),
            "memory region record",
        );
    }
    let info = require(builder.finish(), "boot information validation");
    crate::kernel::run(info)
}

fn require<T, E: core::fmt::Debug>(result: Result<T, E>, stage: &str) -> T {
    result.unwrap_or_else(|error| diagnostics::fatal(format_args!("{stage}: {error:?}")))
}

/// Missing GOP, unsupported masks and modes without direct access are optional.
fn find_framebuffer() -> Option<Framebuffer> {
    let handle = boot::get_handle_for_protocol::<GraphicsOutput>().ok()?;
    let mut gop = boot::open_protocol_exclusive::<GraphicsOutput>(handle).ok()?;
    let info = gop.current_mode_info();
    let (width, height) = info.resolution();
    let (bits_per_pixel, red_bits, red_shift, green_bits, green_shift, blue_bits, blue_shift) =
        match info.pixel_format() {
            PixelFormat::Bgr => (32, 8, 16, 8, 8, 8, 0),
            PixelFormat::Rgb => (32, 8, 0, 8, 8, 8, 16),
            PixelFormat::Bitmask => {
                let mask = info.pixel_bitmask()?;
                let all = mask.red | mask.green | mask.blue | mask.reserved;
                let depth = (32 - all.leading_zeros()) as u8;
                let (red_bits, red_shift) = color_channel(mask.red)?;
                let (green_bits, green_shift) = color_channel(mask.green)?;
                let (blue_bits, blue_shift) = color_channel(mask.blue)?;
                (
                    depth,
                    red_bits,
                    red_shift,
                    green_bits,
                    green_shift,
                    blue_bits,
                    blue_shift,
                )
            }
            PixelFormat::BltOnly => return None,
        };
    let pixel_bytes = usize::from(bits_per_pixel).div_ceil(8);
    let stride_bytes = u32::try_from(info.stride().checked_mul(pixel_bytes)?).ok()?;
    let mut buffer = gop.frame_buffer();
    let framebuffer = Framebuffer {
        physical_address: buffer.as_mut_ptr() as u64,
        byte_len: buffer.size() as u64,
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        stride_bytes,
        bits_per_pixel,
        red_bits,
        green_bits,
        blue_bits,
        red_shift,
        green_shift,
        blue_shift,
    };
    framebuffer.validate().ok()?;
    Some(framebuffer)
}

/// Returns the width and shift of a nonempty contiguous color mask.
fn color_channel(mask: u32) -> Option<(u8, u8)> {
    if mask == 0 {
        return None;
    }
    let shift = mask.trailing_zeros();
    let bits = mask.count_ones();
    let shifted = mask >> shift;
    if shifted & shifted.wrapping_add(1) != 0 {
        return None;
    }
    Some((bits as u8, shift as u8))
}
