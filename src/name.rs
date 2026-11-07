// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Names of tasks and threads.

use core::fmt::{self, Write as _};
use core::sync::atomic::{AtomicU8, Ordering};

/// The bytes a name holds, its final NUL included.
const NAME_SIZE: usize = 32;

/// A task's or thread's name, ending with a NUL.
pub struct Name([AtomicU8; NAME_SIZE]);

impl Name {
    /// An empty name.
    pub const fn new() -> Self {
        Self([const { AtomicU8::new(0) }; NAME_SIZE])
    }

    /// Sets it to `arguments`, cut byte-wise to fit with its final NUL.
    pub fn set(&self, arguments: fmt::Arguments) {
        let mut rest = Rest(&self.0[..NAME_SIZE - 1]);
        let _ = rest.write_fmt(arguments);
        let length = NAME_SIZE - 1 - rest.0.len();
        for byte in &self.0[length..] {
            byte.store(0, Ordering::Relaxed);
        }
    }

    /// Sets it to `other`.
    pub fn copy_from(&self, other: &Self) {
        for (byte, other) in self.0.iter().zip(&other.0) {
            byte.store(other.load(Ordering::Relaxed), Ordering::Relaxed);
        }
    }
}

/// The bytes of a name not yet written, cutting what does not fit.
struct Rest<'a>(&'a [AtomicU8]);

impl fmt::Write for Rest<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        let (written, rest) = self.0.split_at(self.0.len().min(text.len()));
        for (byte, &text) in written.iter().zip(text.as_bytes()) {
            byte.store(text, Ordering::Relaxed);
        }
        self.0 = rest;
        Ok(())
    }
}
