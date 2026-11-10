// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Mach traps: the table system calls index, and the traps written so far.

use crate::arch::{self, Machine as _, PAGE_SIZE};
use crate::error::Error;
use crate::ipc::mach_msg;
use crate::ipc::port::Port;
use crate::{print, smp};

/// A Mach trap, as the table holds it.
pub enum MachTrap {
    /// What it runs, given the caller's arguments, and how many it takes;
    /// its result goes to the caller's rax, zero-extended.
    Written(fn(&[usize; 7]) -> u32, usize),
    /// Not written yet: GNU Mach's name for it.
    NotYet(&'static str),
}

impl MachTrap {
    /// How many arguments it takes; none if not written yet.
    pub const fn argument_count(&self) -> usize {
        match self {
            Self::Written(_, count) => *count,
            Self::NotYet(_) => 0,
        }
    }

    /// Runs it with `arguments`, giving its result.
    ///
    /// # Panics
    ///
    /// If it is not written yet.
    pub fn call(&self, arguments: &[usize; 7]) -> u32 {
        match self {
            Self::Written(function, _) => function(arguments),
            Self::NotYet(name) => panic!("{name}: not yet"),
        }
    }
}

/// How many traps the table holds.
const COUNT: usize = 130;

/// The Mach traps, indexed by the trap number negated; `kern_invalid` for
/// the numbers Mach leaves unused.
pub static MACH_TRAPS: [MachTrap; COUNT] = {
    let mut traps = [const { MachTrap::Written(invalid, 0) }; COUNT];
    traps[10] = MachTrap::NotYet("null_port");
    traps[11] = MachTrap::NotYet("null_port");
    traps[12] = MachTrap::NotYet("null_port");
    traps[13] = MachTrap::NotYet("null_port");
    traps[17] = MachTrap::NotYet("evc_wait");
    traps[18] = MachTrap::NotYet("evc_wait_clear");
    traps[25] = MachTrap::Written(message, 7);
    traps[26] = MachTrap::Written(reply_port, 0);
    traps[27] = MachTrap::NotYet("mach_thread_self");
    traps[28] = MachTrap::NotYet("mach_task_self");
    traps[29] = MachTrap::NotYet("mach_host_self");
    traps[30] = MachTrap::Written(print, 1);
    traps[39] = MachTrap::NotYet("syscall_device_writev_request");
    traps[40] = MachTrap::NotYet("syscall_device_write_request");
    traps[55] = MachTrap::NotYet("null_port");
    traps[56] = MachTrap::NotYet("null_port");
    traps[59] = MachTrap::NotYet("swtch_pri");
    traps[60] = MachTrap::NotYet("swtch");
    traps[61] = MachTrap::NotYet("thread_switch");
    traps[64] = MachTrap::NotYet("syscall_vm_map");
    traps[65] = MachTrap::NotYet("syscall_vm_allocate");
    traps[66] = MachTrap::NotYet("syscall_vm_deallocate");
    traps[68] = MachTrap::NotYet("syscall_task_create");
    traps[69] = MachTrap::NotYet("syscall_task_terminate");
    traps[70] = MachTrap::NotYet("syscall_task_suspend");
    traps[71] = MachTrap::NotYet("syscall_task_set_special_port");
    traps[72] = MachTrap::NotYet("syscall_mach_port_allocate");
    traps[73] = MachTrap::NotYet("syscall_mach_port_deallocate");
    traps[74] = MachTrap::NotYet("syscall_mach_port_insert_right");
    traps[75] = MachTrap::NotYet("syscall_mach_port_allocate_name");
    traps[76] = MachTrap::NotYet("syscall_thread_depress_abort");
    traps[77] = MachTrap::NotYet("thread_set_self_state");
    traps
};

/// A number Mach leaves unused.
fn invalid(_: &[usize; 7]) -> u32 {
    Error::InvalidArgument.into()
}

/// Sends a message and receives one, as `mach_msg_trap`: from the
/// message's address, the option, both sizes, the name received on, the
/// timeout and the notify port, the 32-bit ones in their arguments' low
/// halves.
///
/// # Panics
///
/// As `mach_msg::trap`.
fn message(arguments: &[usize; 7]) -> u32 {
    let [message, rest @ ..] = *arguments;
    let [
        option,
        send_size,
        receive_size,
        receive_name,
        timeout,
        notify,
    ] = rest.map(|argument| u32::try_from(argument & 0xffff_ffff).unwrap());
    mach_msg::trap(
        message,
        option,
        send_size,
        receive_size,
        receive_name,
        timeout,
        notify,
    )
}

/// Gives the caller a reply port: a new port its task's space receives on,
/// under the lowest free name there, which it returns.
///
/// # Panics
///
/// If no thread runs on the CPU. If the port can't be allocated: not yet;
/// as `Port::allocate`.
fn reply_port(_: &[usize; 7]) -> u32 {
    let thread = smp::current()
        .active_thread()
        .expect("mach_reply_port: no current thread");
    Port::allocate(&thread.task().space).expect("mach_reply_port: a failed allocation, not yet")
}

/// How many bytes `print` copies in at a time, at most.
const PRINT_CHUNK: usize = 64;

/// Prints the string at the caller's first argument, copied in by chunks;
/// a fault ends it.
fn print(arguments: &[usize; 7]) -> u32 {
    let mut buffer = [0; PRINT_CHUNK];
    let mut address = arguments[0];
    loop {
        // Within one page, so no byte past the string's end can fault.
        let chunk = &mut buffer[..PRINT_CHUNK.min(PAGE_SIZE - address % PAGE_SIZE)];
        if arch::Current::copy_in(address, chunk).is_err() {
            break;
        }
        let end = chunk.iter().position(|&byte| byte == 0);
        // A character split across chunks prints as U+FFFD, once per piece.
        for piece in chunk[..end.unwrap_or(chunk.len())].utf8_chunks() {
            print!("{}", piece.valid());
            if !piece.invalid().is_empty() {
                print!("{}", char::REPLACEMENT_CHARACTER);
            }
        }
        if end.is_some() {
            break;
        }
        address += chunk.len();
    }
    // What GNU Mach's printf leaves in rax.
    0
}
