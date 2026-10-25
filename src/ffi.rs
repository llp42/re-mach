// SPDX-License-Identifier: MIT
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//! Kernel C interfaces used from Rust.

use core::ffi::{c_char, c_int, c_ulong, c_void};
use core::fmt;

unsafe extern "C" {
    /// Formats to the kernel console; `fmt` is a NUL-terminated format
    /// string whose conversions match the arguments.
    pub fn kprintf(fmt: *const c_char, ...) -> c_int;
    /// Stops the system with a formatted message; same contract as
    /// `kprintf`.
    pub fn panic(fmt: *const c_char, ...) -> !;
    /// The first record of the preloaded module whose type is the
    /// NUL-terminated `kind`, or null.
    pub fn preload_search_by_type(kind: *const c_char) -> *mut c_char;
    /// The data of record `info` of the preloaded `module`, or null.
    pub fn preload_search_info(module: *mut c_char, info: c_int) -> *mut c_char;
    /// Copies `len` bytes from kernel memory to the calling process's user
    /// memory; returns 0 or an errno.
    pub fn copyout(kernel: *const c_void, user: *mut c_void, len: usize) -> c_int;

    /// Makes the mounted root the calling process's root and current
    /// directory; returns 0 or an errno.
    pub fn glue_init_root() -> c_int;
    /// Replaces the calling process's address space with one holding only a
    /// stack; returns 0 or an errno.
    pub fn glue_init_vmspace() -> c_int;
    /// Maps zeroed, writable user memory at exactly `addr..addr + len`, both
    /// page aligned; returns 0 or an errno.
    pub fn glue_user_map(addr: c_ulong, len: c_ulong) -> c_int;
    /// Sets the protection of user memory `addr..addr + len` to `prot`;
    /// returns 0 or an errno.
    pub fn glue_user_protect(addr: c_ulong, len: c_ulong, prot: c_int) -> c_int;
    /// Sets the calling process's registers to start at `entry` with stack
    /// `stack`; it enters user mode when the fork handler returns.
    pub fn glue_user_enter(entry: c_ulong, stack: c_ulong);
    /// The top of the user stack, above every user mapping.
    pub fn glue_user_stack_top() -> c_ulong;
    /// Moves up to `len` bytes that the I/O request `uio` still describes
    /// into `buf`; returns the count, 0 when none remain, or a negated
    /// errno.
    pub fn glue_uio_read(uio: *mut c_void, buf: *mut c_char, len: usize) -> c_int;
}

/// A NUL-terminated string written with `write!`, truncated to fit in `N`
/// bytes.
pub struct CBuf<const N: usize> {
    buf: [u8; N],
    len: usize,
}

impl<const N: usize> CBuf<N> {
    pub const fn new() -> Self {
        Self {
            buf: [0; N],
            len: 0,
        }
    }

    pub fn as_ptr(&self) -> *const c_char {
        self.buf.as_ptr().cast()
    }
}

impl<const N: usize> fmt::Write for CBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        // The last byte stays NUL.
        let n = s.len().min(N - 1 - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}
