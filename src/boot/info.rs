// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from NetBSD
// Upstream-Path: sys/arch/x86/include/bootinfo.h
// Upstream-Commit: cc2030c15de6861749296c2498758dfdfc53eac5
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 1997 Matthias Drochner
//
// Derived from NetBSD
// Upstream-Path: sys/arch/x86/x86/x86_machdep.c
// Upstream-Commit: cc2030c15de6861749296c2498758dfdfc53eac5
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 2002, 2006, 2007 YAMAMOTO Takashi
// SPDX-FileCopyrightText: 2005, 2008, 2009, 2019, 2023 The NetBSD Foundation, Inc.
//
// Derived from NetBSD
// Upstream-Path: sys/arch/i386/stand/lib/bootinfo.c
// Upstream-Commit: cc2030c15de6861749296c2498758dfdfc53eac5
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 1997 Matthias Drochner
//
// Derived from NetBSD
// Upstream-Path: sys/arch/i386/stand/efiboot/efiboot.c
// Upstream-Commit: cc2030c15de6861749296c2498758dfdfc53eac5
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 2016 Kimihiro Nonaka <nonaka@netbsd.org>
//! Owned-by-caller boot records, encoded field by field in little endian.
//!
//! Each record is `[length: u32, kind: u32, payload]`, without alignment gaps.
//! Length includes the eight-byte header. Unknown kinds are skipped; malformed
//! records and duplicate singleton records are errors. Borrowing the blob keeps
//! its storage immutable for the lifetime of the validated view.

use core::ops::Range;

const HEADER_SIZE: usize = 8;
const IMAGE: u32 = 1;
const MEMORY_REGION: u32 = 2;
const FRAMEBUFFER: u32 = 12;
const FRAMEBUFFER_SIZE: usize = 35;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Capacity,
    Truncated,
    InvalidRecord,
    DuplicateRecord,
    MissingRequiredRecord,
    InvalidRange,
    InvalidFramebuffer,
}

/// Availability at handoff, not permission to reclaim firmware or loader data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MemoryKind {
    Reserved,
    Available,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MemoryRegion {
    pub physical_start: u64,
    pub byte_len: u64,
    pub kind: MemoryKind,
}

/// A directly accessible framebuffer; stride and allocation length are bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Framebuffer {
    pub physical_address: u64,
    pub byte_len: u64,
    pub width: u32,
    pub height: u32,
    pub stride_bytes: u32,
    pub bits_per_pixel: u8,
    pub red_bits: u8,
    pub green_bits: u8,
    pub blue_bits: u8,
    pub red_shift: u8,
    pub green_shift: u8,
    pub blue_shift: u8,
}

impl Framebuffer {
    pub fn validate(&self) -> Result<(), Error> {
        validate_range(self.physical_address, self.byte_len)?;
        let pixel_bytes = u64::from(self.bits_per_pixel).div_ceil(8);
        if self.width == 0
            || self.height == 0
            || !(1..=32).contains(&self.bits_per_pixel)
            || u64::from(self.stride_bytes) < u64::from(self.width) * pixel_bytes
            || u64::from(self.stride_bytes) * u64::from(self.height) > self.byte_len
        {
            return Err(Error::InvalidFramebuffer);
        }
        let mut used = 0_u64;
        for (bits, shift) in [
            (self.red_bits, self.red_shift),
            (self.green_bits, self.green_shift),
            (self.blue_bits, self.blue_shift),
        ] {
            if bits == 0 || u16::from(bits) + u16::from(shift) > u16::from(self.bits_per_pixel) {
                return Err(Error::InvalidFramebuffer);
            }
            let mask = ((1_u64 << bits) - 1) << shift;
            if used & mask != 0 {
                return Err(Error::InvalidFramebuffer);
            }
            used |= mask;
        }
        Ok(())
    }
}

/// A validated view; no global registration or firmware references are needed.
#[derive(Debug)]
pub struct BootInfo<'a> {
    bytes: &'a [u8],
    image: Range<u64>,
    framebuffer: Option<Framebuffer>,
}

impl<'a> BootInfo<'a> {
    /// Validates the whole blob, including records following known entries.
    /// Requires one image and at least one memory region; framebuffer is optional.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, Error> {
        let mut rest = bytes;
        let mut image = None;
        let mut framebuffer = None;
        let mut has_memory = false;
        while !rest.is_empty() {
            let (kind, payload) = next_record(&mut rest)?;
            match kind {
                IMAGE => {
                    if image.is_some() {
                        return Err(Error::DuplicateRecord);
                    }
                    if payload.len() != 16 {
                        return Err(Error::InvalidRecord);
                    }
                    let start = read_u64(payload, 0)?;
                    let end = read_u64(payload, 8)?;
                    if start >= end {
                        return Err(Error::InvalidRange);
                    }
                    image = Some(start..end);
                }
                MEMORY_REGION => {
                    decode_region(payload)?;
                    has_memory = true;
                }
                FRAMEBUFFER => {
                    if framebuffer.is_some() {
                        return Err(Error::DuplicateRecord);
                    }
                    framebuffer = Some(decode_framebuffer(payload)?);
                }
                _ => {}
            }
        }
        if !has_memory {
            return Err(Error::MissingRequiredRecord);
        }
        Ok(Self {
            bytes,
            image: image.ok_or(Error::MissingRequiredRecord)?,
            framebuffer,
        })
    }

    /// Physical image extent; its end does not imply adjacent memory is free.
    pub fn image(&self) -> Range<u64> {
        self.image.clone()
    }

    pub fn framebuffer(&self) -> Option<Framebuffer> {
        self.framebuffer
    }

    pub fn memory_regions(&self) -> impl Iterator<Item = MemoryRegion> + '_ {
        let mut rest = self.bytes;
        core::iter::from_fn(move || {
            while !rest.is_empty() {
                // The entire immutable blob was validated by parse.
                let (kind, payload) = next_record(&mut rest).expect("validated record");
                if kind == MEMORY_REGION {
                    return Some(decode_region(payload).expect("validated memory region"));
                }
            }
            None
        })
    }
}

/// Builds records in caller-owned storage. Capacity failure leaves prior records
/// intact. Finish validates the complete handoff before exposing it to the kernel.
pub struct Builder<'a> {
    bytes: &'a mut [u8],
    len: usize,
}

impl<'a> Builder<'a> {
    pub fn new(bytes: &'a mut [u8]) -> Self {
        Self { bytes, len: 0 }
    }

    pub fn image(&mut self, image: Range<u64>) -> Result<(), Error> {
        if image.start >= image.end {
            return Err(Error::InvalidRange);
        }
        let mut payload = [0; 16];
        payload[..8].copy_from_slice(&image.start.to_le_bytes());
        payload[8..].copy_from_slice(&image.end.to_le_bytes());
        self.record(IMAGE, &payload)
    }

    pub fn memory_region(&mut self, region: MemoryRegion) -> Result<(), Error> {
        validate_range(region.physical_start, region.byte_len)?;
        let mut payload = [0; 20];
        payload[..8].copy_from_slice(&region.physical_start.to_le_bytes());
        payload[8..16].copy_from_slice(&region.byte_len.to_le_bytes());
        let kind: u32 = match region.kind {
            MemoryKind::Reserved => 0,
            MemoryKind::Available => 1,
        };
        payload[16..].copy_from_slice(&kind.to_le_bytes());
        self.record(MEMORY_REGION, &payload)
    }

    pub fn framebuffer(&mut self, framebuffer: Framebuffer) -> Result<(), Error> {
        framebuffer.validate()?;
        let mut payload = [0; FRAMEBUFFER_SIZE];
        payload[..8].copy_from_slice(&framebuffer.physical_address.to_le_bytes());
        payload[8..16].copy_from_slice(&framebuffer.byte_len.to_le_bytes());
        payload[16..20].copy_from_slice(&framebuffer.width.to_le_bytes());
        payload[20..24].copy_from_slice(&framebuffer.height.to_le_bytes());
        payload[24..28].copy_from_slice(&framebuffer.stride_bytes.to_le_bytes());
        payload[28..].copy_from_slice(&[
            framebuffer.bits_per_pixel,
            framebuffer.red_bits,
            framebuffer.green_bits,
            framebuffer.blue_bits,
            framebuffer.red_shift,
            framebuffer.green_shift,
            framebuffer.blue_shift,
        ]);
        self.record(FRAMEBUFFER, &payload)
    }

    pub fn finish(self) -> Result<BootInfo<'a>, Error> {
        BootInfo::parse(&self.bytes[..self.len])
    }

    fn record(&mut self, kind: u32, payload: &[u8]) -> Result<(), Error> {
        let length = HEADER_SIZE
            .checked_add(payload.len())
            .ok_or(Error::Capacity)?;
        let encoded_length = u32::try_from(length).map_err(|_| Error::Capacity)?;
        let end = self.len.checked_add(length).ok_or(Error::Capacity)?;
        let record = self.bytes.get_mut(self.len..end).ok_or(Error::Capacity)?;
        record[..4].copy_from_slice(&encoded_length.to_le_bytes());
        record[4..8].copy_from_slice(&kind.to_le_bytes());
        record[8..].copy_from_slice(payload);
        self.len = end;
        Ok(())
    }
}

fn next_record<'a>(rest: &mut &'a [u8]) -> Result<(u32, &'a [u8]), Error> {
    let length = read_u32(rest, 0)? as usize;
    let kind = read_u32(rest, 4)?;
    if length < HEADER_SIZE {
        return Err(Error::InvalidRecord);
    }
    let payload = rest.get(HEADER_SIZE..length).ok_or(Error::Truncated)?;
    *rest = &rest[length..];
    Ok((kind, payload))
}

fn decode_region(payload: &[u8]) -> Result<MemoryRegion, Error> {
    if payload.len() != 20 {
        return Err(Error::InvalidRecord);
    }
    let physical_start = read_u64(payload, 0)?;
    let byte_len = read_u64(payload, 8)?;
    validate_range(physical_start, byte_len)?;
    let kind = match read_u32(payload, 16)? {
        0 => MemoryKind::Reserved,
        1 => MemoryKind::Available,
        _ => return Err(Error::InvalidRecord),
    };
    Ok(MemoryRegion {
        physical_start,
        byte_len,
        kind,
    })
}

fn decode_framebuffer(payload: &[u8]) -> Result<Framebuffer, Error> {
    if payload.len() != FRAMEBUFFER_SIZE {
        return Err(Error::InvalidRecord);
    }
    let framebuffer = Framebuffer {
        physical_address: read_u64(payload, 0)?,
        byte_len: read_u64(payload, 8)?,
        width: read_u32(payload, 16)?,
        height: read_u32(payload, 20)?,
        stride_bytes: read_u32(payload, 24)?,
        bits_per_pixel: payload[28],
        red_bits: payload[29],
        green_bits: payload[30],
        blue_bits: payload[31],
        red_shift: payload[32],
        green_shift: payload[33],
        blue_shift: payload[34],
    };
    framebuffer.validate()?;
    Ok(framebuffer)
}

fn validate_range(start: u64, length: u64) -> Result<(), Error> {
    if length == 0 || start.checked_add(length).is_none() {
        return Err(Error::InvalidRange);
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> Result<u32, Error> {
    let bytes = bytes.get(offset..offset + 4).ok_or(Error::Truncated)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_u64(bytes: &[u8], offset: usize) -> Result<u64, Error> {
    let bytes = bytes.get(offset..offset + 8).ok_or(Error::Truncated)?;
    Ok(u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]))
}
