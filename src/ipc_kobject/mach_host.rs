// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The host calls' server routines, as MIG makes them from `mach_host`'s
//! definitions; only those written so far.

use super::mig::{self, StringC};
use crate::error::Error;
use crate::host::{self, Host};
use crate::ipc::MessageType;
use crate::{clock, ipc_host, machine};

mig::subsystem! {
    base 2600;
    34 host_get_time(host = ipc_host::host) -> (current_time: RpcTimeValue);
    35 host_reboot(host = ipc_host::privileged_host, options: i32);
    38 host_info(host = ipc_host::host, flavor: i32) -> (info: [i32; ..1024], CountInOut);
    43 host_get_time64(host = ipc_host::host) -> (current_time: TimeValue64);
    46 host_get_kernel_version(host = ipc_host::host) -> (kernel_version: StringC<512>);
    47 host_get_uptime64(host = ipc_host::host) -> (uptime: TimeValue64);
}

/// Mach's `rpc_time_value_t`, as MIG carries it.
#[derive(Clone, Copy)]
#[repr(C)]
struct RpcTimeValue {
    /// Its seconds.
    seconds: i64,
    /// Its microseconds.
    microseconds: i32,
    /// Padding to 8 bytes.
    padding: [u8; 4],
}

// SAFETY: `repr(C)`, integers only, no padding.
unsafe impl mig::Inline for RpcTimeValue {
    const WORD: u64 = mig::type_word(MessageType::Integer64, 64, 2);
}

/// Mach's `time_value64_t`, as MIG carries it.
#[derive(Clone, Copy)]
#[repr(C)]
struct TimeValue64 {
    /// Its seconds.
    seconds: i64,
    /// Its nanoseconds.
    nanoseconds: i64,
}

// SAFETY: `repr(C)`, integers only, no padding.
unsafe impl mig::Inline for TimeValue64 {
    const WORD: u64 = mig::type_word(MessageType::Integer64, 64, 2);
}

/// Serves `host_get_time`: the time of day for `host`.
///
/// # Panics
///
/// As `clock::time::host_time`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn host_get_time(host: Option<&'static Host>) -> Result<RpcTimeValue, Error> {
    let time = clock::time::host_time(host);
    Ok(RpcTimeValue {
        seconds: time.seconds(),
        microseconds: time.microseconds(),
        padding: [0; 4],
    })
}

/// Serves `host_reboot`; it doesn't return.
///
/// # Panics
///
/// As `machine::host_reboot`.
fn host_reboot(host: Option<&'static Host>, options: i32) -> Result<(), Error> {
    machine::host_reboot(host, options)
}

/// Serves `host_info`: up to `info.len()` integers of `flavor` for `host`.
///
/// # Panics
///
/// As `host::info`.
fn host_info(host: Option<&'static Host>, flavor: i32, info: &mut [i32]) -> Result<usize, Error> {
    host::info(host, flavor, info)
}

/// Serves `host_get_time64`: the time of day for `host`.
///
/// # Panics
///
/// As `clock::time::host_time64`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn host_get_time64(host: Option<&'static Host>) -> Result<TimeValue64, Error> {
    let time = clock::time::host_time64(host);
    Ok(TimeValue64 {
        seconds: time.seconds,
        nanoseconds: time.nanoseconds,
    })
}

/// Serves `host_get_kernel_version`: the kernel's version for `host`.
fn host_get_kernel_version(host: Option<&'static Host>) -> Result<StringC<512>, Error> {
    let mut kernel_version = StringC([0; 512]);
    host::kernel_version(host, &mut kernel_version.0)?;
    Ok(kernel_version)
}

/// Serves `host_get_uptime64`: the time since boot for `host`.
///
/// # Panics
///
/// As `clock::time::host_uptime64`.
#[expect(clippy::unnecessary_wraps, reason = "MIG's routines give a Result")]
fn host_get_uptime64(host: Option<&'static Host>) -> Result<TimeValue64, Error> {
    let uptime = clock::time::host_uptime64(host);
    Ok(TimeValue64 {
        seconds: uptime.seconds,
        nanoseconds: uptime.nanoseconds,
    })
}
