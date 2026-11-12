// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Host information.

use core::ptr;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::config::MAX_CPUS;
use crate::error::Error;
use crate::ipc::port::Port;
use crate::{VERSION, clock, machine, sched};

/// The host: the machine as a whole; so far only its ports.
pub struct Host {
    /// Its port, for information about it.
    pub(super) port: AtomicPtr<Port>,
    /// Its privileged port, for controlling it.
    pub(super) privileged_port: AtomicPtr<Port>,
}

/// The host, whose ports `ipc_host::init` makes.
pub static HOST: Host = Host {
    port: AtomicPtr::new(ptr::null_mut()),
    privileged_port: AtomicPtr::new(ptr::null_mut()),
};

/// Writes the kernel's version for `host` into `version`, as
/// `host_get_kernel_version` does: `VERSION`, then NULs to the end.
///
/// # Errors
///
/// `InvalidArgument` with no host.
pub fn kernel_version(host: Option<&Host>, version: &mut [u8; 512]) -> Result<(), Error> {
    if host.is_none() {
        return Err(Error::InvalidArgument);
    }
    // As strncpy: the string, cut to fit, then NULs.
    let length = VERSION.len().min(version.len());
    let (string, padding) = version.split_at_mut(length);
    string.copy_from_slice(&VERSION.as_bytes()[..length]);
    padding.fill(0);
    Ok(())
}

/// What `info` reports, as Mach numbers it; only those written so far.
enum Flavor {
    /// `HOST_BASIC_INFO`: the CPUs, the memory and the processor type.
    Basic,
    /// `HOST_PROCESSOR_SLOTS`: the slots of the processors up.
    ProcessorSlots,
    /// `HOST_SCHED_INFO`: the shortest timeout and quantum.
    Sched,
    /// `HOST_LOAD_INFO`: the load averages and Mach factors.
    Load,
}

impl TryFrom<i32> for Flavor {
    type Error = i32;

    /// The flavor of Mach's number `number`, if it is one written here.
    fn try_from(number: i32) -> Result<Self, i32> {
        match number {
            1 => Ok(Self::Basic),
            2 => Ok(Self::ProcessorSlots),
            3 => Ok(Self::Sched),
            4 => Ok(Self::Load),
            _ => Err(number),
        }
    }
}

/// `HOST_BASIC_INFO`'s record, as Mach lays it out.
#[repr(C)]
struct BasicInfo {
    /// The most CPUs the kernel supports.
    max_cpus: i32,
    /// How many processors are up.
    available_cpus: i32,
    /// The bytes of memory.
    memory_size: u64,
    /// The boot processor's type.
    cpu_type: i32,
    /// The boot processor's subtype.
    cpu_subtype: i32,
}

/// `HOST_SCHED_INFO`'s record, as Mach lays it out.
#[repr(C)]
struct SchedInfo {
    /// The shortest timeout, in milliseconds.
    min_timeout: i32,
    /// The shortest quantum, in milliseconds.
    min_quantum: i32,
}

/// `HOST_LOAD_INFO`'s record, as Mach lays it out.
#[repr(C)]
struct LoadInfo {
    /// The load averages, by `LOAD_SCALE`.
    avenrun: [i32; 3],
    /// The Mach factors, by `LOAD_SCALE`.
    mach_factor: [i32; 3],
}

/// Writes `record` at the start of `info`, as a flavor's integers; gives
/// their count, or `None` if `info` is too short.
///
/// # Safety
///
/// `T` is 32- and 64-bit integers alone, with no padding.
unsafe fn fill<T>(info: &mut [i32], record: T) -> Option<usize> {
    let count = size_of::<T>() / size_of::<i32>();
    if info.len() < count {
        return None;
    }
    // SAFETY: `info` holds `count` integers, a `T`'s bytes; the caller's
    // `T`, 32- and 64-bit integers with no padding, sets all of them.
    unsafe { info.as_mut_ptr().cast::<T>().write_unaligned(record) };
    Some(count)
}

/// Writes `host`'s information of `flavor` into `info`, as `host_info`
/// does; `info`'s length is the count asked for. Gives the count written.
///
/// # Errors
///
/// `InvalidArgument` with no host.
///
/// # Panics
///
/// For a count too short for the flavor, or a flavor not written here: not
/// yet.
pub fn info(host: Option<&Host>, flavor: i32, info: &mut [i32]) -> Result<usize, Error> {
    if host.is_none() {
        return Err(Error::InvalidArgument);
    }
    match Flavor::try_from(flavor) {
        Ok(Flavor::Basic) => {
            let machine = machine::info();
            // The boot processor's slot.
            let slot = machine::slot(0).expect("host_info: no boot processor slot");
            let basic = BasicInfo {
                max_cpus: machine.max_cpus(),
                available_cpus: i32::try_from(machine::available_cpus()).unwrap(),
                memory_size: u64::try_from(machine.memory_size()).unwrap(),
                cpu_type: slot.ty.0,
                cpu_subtype: slot.subtype.0,
            };
            // SAFETY: 32- and 64-bit integers alone, with no padding.
            let count = unsafe { fill(info, basic) };
            Ok(count.expect("host_info: HOST_BASIC_INFO, KERN_FAILURE, not yet"))
        }
        Ok(Flavor::ProcessorSlots) => {
            assert!(
                info.len() >= MAX_CPUS,
                "host_info: HOST_PROCESSOR_SLOTS, KERN_INVALID_ARGUMENT, not yet"
            );
            let running = (0..MAX_CPUS).filter(|&cpu| {
                machine::slot(cpu).is_some_and(|slot| slot.running.load(Ordering::Acquire))
            });
            let mut count = 0;
            for (entry, cpu) in info.iter_mut().zip(running) {
                *entry = i32::try_from(cpu).unwrap();
                count += 1;
            }
            Ok(count)
        }
        Ok(Flavor::Sched) => {
            let tick = i32::try_from(clock::TICK_MICROSECONDS).unwrap();
            let sched = SchedInfo {
                min_timeout: tick / 1000,
                min_quantum: i32::try_from(sched::MIN_QUANTUM).unwrap() * tick / 1000,
            };
            // SAFETY: 32-bit integers alone, with no padding.
            let count = unsafe { fill(info, sched) };
            Ok(count.expect("host_info: HOST_SCHED_INFO, KERN_FAILURE, not yet"))
        }
        Ok(Flavor::Load) => {
            let load = LoadInfo {
                avenrun: sched::load_averages().map(|average| i32::try_from(average).unwrap()),
                mach_factor: sched::mach_factors().map(|factor| i32::try_from(factor).unwrap()),
            };
            // SAFETY: 32-bit integers alone, with no padding.
            let count = unsafe { fill(info, load) };
            Ok(count.expect("host_info: HOST_LOAD_INFO, KERN_FAILURE, not yet"))
        }
        Err(5) => panic!("host_info: HOST_EFI_SYSTAB, not yet"),
        Err(flavor) => panic!("host_info: flavor {flavor}, KERN_INVALID_ARGUMENT, not yet"),
    }
}

/// The EFI system table address `host_info` reports when UEFI is not
/// available.
#[expect(dead_code, reason = "host_info reports it")]
pub const HOST_NO_UEFI: u64 = u64::MAX;
