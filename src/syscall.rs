// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Mach traps: the table system calls index, and the traps written so far.

use crate::arch::{self, Machine as _, PAGE_SIZE};
use crate::error::Error;
use crate::print;

/// A Mach trap, as the table holds it.
pub enum MachTrap {
    /// What it runs, given the caller's register arguments; its result goes
    /// to the caller's rax, zero-extended.
    Written(fn(&[usize; 6]) -> u32),
    /// Not written yet: GNU Mach's name for it.
    NotYet(&'static str),
}

impl MachTrap {
    /// Runs it with `arguments`, giving its result.
    ///
    /// # Panics
    ///
    /// If it is not written yet.
    pub fn call(&self, arguments: &[usize; 6]) -> u32 {
        match self {
            Self::Written(function) => function(arguments),
            Self::NotYet(name) => panic!("{name}: not yet"),
        }
    }
}

/// How many traps the table holds.
const COUNT: usize = 130;

/// The Mach traps, indexed by the trap number negated; `kern_invalid` for
/// the numbers Mach leaves unused.
pub static MACH_TRAPS: [MachTrap; COUNT] = {
    let mut traps = [const { MachTrap::Written(invalid) }; COUNT];
    traps[10] = MachTrap::NotYet("null_port");
    traps[11] = MachTrap::NotYet("null_port");
    traps[12] = MachTrap::NotYet("null_port");
    traps[13] = MachTrap::NotYet("null_port");
    traps[17] = MachTrap::NotYet("evc_wait");
    traps[18] = MachTrap::NotYet("evc_wait_clear");
    traps[25] = MachTrap::NotYet("mach_msg_trap");
    traps[26] = MachTrap::Written(reply_port);
    traps[27] = MachTrap::NotYet("mach_thread_self");
    traps[28] = MachTrap::NotYet("mach_task_self");
    traps[29] = MachTrap::NotYet("mach_host_self");
    traps[30] = MachTrap::Written(print);
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
fn invalid(_: &[usize; 6]) -> u32 {
    Error::InvalidArgument.into()
}

/// Gives the caller a reply port: not written yet, so the run halts here,
/// where it grows next.
fn reply_port(_: &[usize; 6]) -> u32 {
    arch::Current::halt()
}

/// How many bytes `print` copies in at a time, at most.
const PRINT_CHUNK: usize = 64;

/// Prints the string at the caller's first argument, copied in by chunks;
/// a fault ends it.
fn print(arguments: &[usize; 6]) -> u32 {
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
