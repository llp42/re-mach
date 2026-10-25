// SPDX-License-Identifier: BSD-2-Clause AND BSD-3-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from DragonFly BSD
// Upstream-Path: stand/boot/efi/loader/bootinfo.c
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 1998 Michael Smith <msmith@freebsd.org>
// SPDX-FileCopyrightText: 2004, 2006 Marcel Moolenaar
// SPDX-FileCopyrightText: 2014 The FreeBSD Foundation
//
// Derived from DragonFly BSD
// Upstream-Path: sys/sys/linker.h
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 1997 Doug Rabson
//
// Derived from DragonFly BSD
// Upstream-Path: sys/platform/pc64/include/metadata.h
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-2-Clause
// SPDX-FileCopyrightText: 2003 Peter Wemm <peter@FreeBSD.org>
// SPDX-FileCopyrightText: 2008 The DragonFly Project.
//
// Derived from DragonFly BSD
// Upstream-Path: sys/sys/reboot.h
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 1982, 1986, 1988, 1993, 1994 The Regents of the University of California.
//! The kernel's preload metadata and environment, encoded into memory the
//! caller reserved for them.
//!
//! Metadata is a list of records `[kind: u32, size: u32, data]` in little
//! endian. Each record starts 8-byte aligned relative to the buffer, and
//! the list ends with a record of kind and size zero. The environment is a
//! list of `name=value` strings, each ending in NUL, followed by an empty
//! string.

use core::fmt::{self, Write};

/// The module's name, a NUL-terminated string; the first record.
pub const NAME: u32 = 0x0001;
/// The module's type, a NUL-terminated string.
pub const TYPE: u32 = 0x0002;
/// The module's physical load address, `u64`.
pub const ADDRESS: u32 = 0x0003;
/// The module's size in bytes, `u64`.
pub const SIZE: u32 = 0x0004;
const METADATA: u32 = 0x8000;
/// Physical address of the environment, `u64`.
pub const ENVIRONMENT: u32 = METADATA | 0x0006;
/// Boot flags, `u32`.
pub const BOOT_FLAGS: u32 = METADATA | 0x0007;
/// The physical memory map: packed entries of `base: u64`, `length: u64`
/// and `kind: u32`, where kind 1 is usable memory.
pub const MEMORY_MAP: u32 = METADATA | 0x1001;

/// Boot flag: the serial port is the console, kept at its current speed.
pub const SERIAL_CONSOLE: u32 = 0x1000;

const RECORD_ALIGN: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Capacity,
    InvalidString,
}

/// Metadata writer over caller-owned storage. The storage keeps prior
/// records when a later one does not fit.
pub struct Metadata<'a> {
    buffer: &'a mut [u8],
    len: usize,
}

impl<'a> Metadata<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, len: 0 }
    }

    /// Adds a NUL-terminated string record; `value` must not contain NUL.
    pub fn string(&mut self, kind: u32, value: &str) -> Result<(), Error> {
        if value.contains('\0') {
            return Err(Error::InvalidString);
        }
        let data = self.record(kind, value.len() + 1)?;
        data[..value.len()].copy_from_slice(value.as_bytes());
        Ok(())
    }

    pub fn u32(&mut self, kind: u32, value: u32) -> Result<(), Error> {
        self.record(kind, 4)?.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    pub fn u64(&mut self, kind: u32, value: u64) -> Result<(), Error> {
        self.record(kind, 8)?.copy_from_slice(&value.to_le_bytes());
        Ok(())
    }

    /// Adds a record of `size` bytes and returns its zeroed data for the
    /// caller to fill.
    pub fn record(&mut self, kind: u32, size: usize) -> Result<&mut [u8], Error> {
        let size_field = u32::try_from(size).map_err(|_| Error::Capacity)?;
        let data_start = self.len + 8;
        let end = size
            .checked_next_multiple_of(RECORD_ALIGN)
            .and_then(|padded| data_start.checked_add(padded))
            .filter(|end| *end <= self.buffer.len())
            .ok_or(Error::Capacity)?;
        self.buffer[self.len..self.len + 4].copy_from_slice(&kind.to_le_bytes());
        self.buffer[self.len + 4..data_start].copy_from_slice(&size_field.to_le_bytes());
        self.buffer[data_start..end].fill(0);
        self.len = end;
        Ok(&mut self.buffer[data_start..data_start + size])
    }

    /// Ends the list and returns its length in bytes.
    pub fn finish(mut self) -> Result<usize, Error> {
        self.record(0, 0)?;
        Ok(self.len)
    }
}

/// Environment writer over caller-owned storage.
pub struct Environment<'a> {
    buffer: &'a mut [u8],
    len: usize,
}

impl<'a> Environment<'a> {
    pub fn new(buffer: &'a mut [u8]) -> Self {
        Self { buffer, len: 0 }
    }

    /// Adds `name=value`. Neither may contain NUL, and `name` may not
    /// contain `=`. A failed entry leaves the list as it was.
    pub fn set(&mut self, name: &str, value: fmt::Arguments<'_>) -> Result<(), Error> {
        if name.contains('=') {
            return Err(Error::InvalidString);
        }
        let start = self.len;
        let result = self
            .append(format_args!("{name}={value}"))
            .and_then(|()| self.push(0));
        if result.is_err() {
            self.len = start;
        }
        result
    }

    /// Ends the list and returns its length in bytes.
    pub fn finish(mut self) -> Result<usize, Error> {
        self.push(0)?;
        Ok(self.len)
    }

    fn push(&mut self, byte: u8) -> Result<(), Error> {
        *self.buffer.get_mut(self.len).ok_or(Error::Capacity)? = byte;
        self.len += 1;
        Ok(())
    }

    fn append(&mut self, text: fmt::Arguments<'_>) -> Result<(), Error> {
        let mut writer = Appender {
            buffer: self.buffer,
            len: self.len,
            error: None,
        };
        let _ = writer.write_fmt(text);
        if let Some(error) = writer.error {
            return Err(error);
        }
        self.len = writer.len;
        Ok(())
    }
}

/// Appends formatted text that contains no NUL.
struct Appender<'b> {
    buffer: &'b mut [u8],
    len: usize,
    error: Option<Error>,
}

impl Write for Appender<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if text.contains('\0') {
            self.error = Some(Error::InvalidString);
            return Err(fmt::Error);
        }
        let Some(target) = self.buffer.get_mut(self.len..self.len + text.len()) else {
            self.error = Some(Error::Capacity);
            return Err(fmt::Error);
        };
        target.copy_from_slice(text.as_bytes());
        self.len += text.len();
        Ok(())
    }
}
