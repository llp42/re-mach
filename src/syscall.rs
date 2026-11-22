// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Mach traps: the table system calls index, and the traps written so far.

use core::mem::ManuallyDrop;
use core::ptr::NonNull;
use core::sync::atomic::Ordering;

use crate::arch::{self, Machine as _, PAGE_SIZE};
use crate::error::Error;
use crate::host::HOST;
use crate::ipc::mach_msg;
use crate::ipc::port::{Port, PortName};
use crate::reference::Reference;
use crate::thread::machine::MachineThread as _;
use crate::{print, smp};

/// A Mach trap, as the table holds it.
pub enum MachTrap {
    /// What it runs, given the caller's arguments, and how many it takes;
    /// its result goes to the caller's rax, zero-extended.
    Written(fn(&[usize; 7]) -> u32, usize),
    /// The same, for a trap that may set the caller's registers itself:
    /// with no result, they stay as it set them.
    SetsRegisters(fn(&[usize; 7]) -> Option<u32>, usize),
    /// Not written yet: GNU Mach's name for it.
    NotYet(&'static str),
}

impl MachTrap {
    /// How many arguments it takes; none if not written yet.
    pub const fn argument_count(&self) -> usize {
        match self {
            Self::Written(_, count) | Self::SetsRegisters(_, count) => *count,
            Self::NotYet(_) => 0,
        }
    }

    /// Runs it with `arguments`, giving its result for the caller's rax, if
    /// any.
    ///
    /// # Panics
    ///
    /// If it is not written yet.
    pub fn call(&self, arguments: &[usize; 7]) -> Option<u32> {
        match self {
            Self::Written(function, _) => Some(function(arguments)),
            Self::SetsRegisters(function, _) => function(arguments),
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
    traps[27] = MachTrap::Written(thread_self, 0);
    traps[28] = MachTrap::Written(task_self, 0);
    traps[29] = MachTrap::Written(host_self, 0);
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
    traps[72] = MachTrap::Written(allocate_port, 3);
    traps[73] = MachTrap::NotYet("syscall_mach_port_deallocate");
    traps[74] = MachTrap::NotYet("syscall_mach_port_insert_right");
    traps[75] = MachTrap::NotYet("syscall_mach_port_allocate_name");
    traps[76] = MachTrap::NotYet("syscall_thread_depress_abort");
    traps[77] = MachTrap::SetsRegisters(set_self_state, 3);
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
        PortName::from(receive_name),
        timeout,
        PortName::from(notify),
    )
}

/// Gives the caller a reply port: a new port its task's space receives on,
/// under a free name there, which it returns.
///
/// # Panics
///
/// If no thread runs on the CPU. If the port can't be allocated: not yet;
/// as `Port::allocate`.
fn reply_port(_: &[usize; 7]) -> u32 {
    let thread = smp::current()
        .active_thread()
        .expect("mach_reply_port: no current thread");
    Port::allocate(&thread.task().space)
        .expect("mach_reply_port: a failed allocation, not yet")
        .into()
}

/// Gives the caller a send right for its own port, as `mach_thread_self`:
/// its name in its task's space.
///
/// # Panics
///
/// If no thread runs on the CPU; as `Thread::retrieve_self` and
/// `Port::copy_out_send`.
fn thread_self(_: &[usize; 7]) -> u32 {
    let thread = smp::current()
        .active_thread()
        .expect("mach_thread_self: no current thread");
    Port::copy_out_send(thread.retrieve_self(), &thread.task().space).into()
}

/// Gives the caller a send right for its task's port, as `mach_task_self`:
/// its name in the task's space.
///
/// # Panics
///
/// If no thread runs on the CPU; as `Task::retrieve_self` and
/// `Port::copy_out_send`.
fn task_self(_: &[usize; 7]) -> u32 {
    let thread = smp::current()
        .active_thread()
        .expect("mach_task_self: no current thread");
    let task = thread.task();
    Port::copy_out_send(task.retrieve_self(), &task.space).into()
}

/// Gives the caller a send right for the host's port, as `mach_host_self`:
/// its name in the caller's task's space.
///
/// # Panics
///
/// If the host has no port, or no thread runs on the CPU; as
/// `Port::copy_out_send`.
fn host_self(_: &[usize; 7]) -> u32 {
    let port =
        NonNull::new(HOST.port.load(Ordering::Acquire)).expect("mach_host_self: no host port");
    // SAFETY: the host's slot keeps its port's reference for good, which
    // the handle borrows and never drops.
    let port = ManuallyDrop::new(unsafe { Reference::from_raw(port) });
    let send = Port::make_send(&port);
    let thread = smp::current()
        .active_thread()
        .expect("mach_host_self: no current thread");
    Port::copy_out_send(send, &thread.task().space).into()
}

/// Puts a new right in a task's space under a free name, as
/// `syscall_mach_port_allocate`: the task its first argument's low half
/// names, the kind its second's numbers, the name copied out to its third,
/// a fault ignored.
///
/// # Panics
///
/// If no thread runs on the CPU. If the name is for a dead port or one of
/// no task: not yet. As `Space::task_space` and `Space::allocate`.
fn allocate_port(arguments: &[usize; 7]) -> u32 {
    let thread = smp::current()
        .active_thread()
        .expect("syscall_mach_port_allocate: no current thread");
    let [task, right] =
        [arguments[0], arguments[1]].map(|argument| u32::try_from(argument & 0xffff_ffff).unwrap());
    let space = thread
        .task()
        .space
        .task_space(PortName::from(task))
        .unwrap_or_else(|| panic!("syscall_mach_port_allocate: MACH_SEND_INTERRUPTED, not yet"));
    match space.allocate(right) {
        Ok(name) => {
            let _ = arch::Current::copy_out(&u32::from(name).to_ne_bytes(), arguments[2]);
            0
        }
        Err(error) => error.into(),
    }
}

/// Sets the caller's own state of the flavor in its first argument's low
/// half from the words at its second, as many as its third's low half, as
/// `thread_set_self_state`: copied in first. Gives no result once set, so
/// the caller returns to user mode with that state, else the error.
///
/// # Panics
///
/// If no thread runs on the CPU; as `MachineThread::write_own_state`.
fn set_self_state(arguments: &[usize; 7]) -> Option<u32> {
    /// The most words a state copied in may have.
    const MAX_COUNT: usize = 150;
    let thread = smp::current()
        .active_thread()
        .expect("thread_set_self_state: no current thread");
    let flavor = i32::from_le_bytes(
        u32::try_from(arguments[0] & 0xffff_ffff)
            .unwrap()
            .to_le_bytes(),
    );
    let count = arguments[2] & 0xffff_ffff;
    if count == 0 || count > MAX_COUNT {
        return Some(Error::InvalidArgument.into());
    }
    let mut bytes = [0; 4 * MAX_COUNT];
    let bytes = &mut bytes[..4 * count];
    if arch::Current::copy_in(arguments[1], bytes).is_err() {
        return Some(Error::InvalidArgument.into());
    }
    let mut state = [0; MAX_COUNT];
    for (word, chunk) in state.iter_mut().zip(bytes.as_chunks::<4>().0) {
        *word = u32::from_ne_bytes(*chunk);
    }
    // SAFETY: the current thread, in its system call, whose user registers
    // nothing borrows while a trap runs.
    let result = unsafe {
        thread
            .machine_state()
            .write_own_state(flavor, &state[..count])
    };
    // Set, no result: back to user mode with the state, as
    // thread_exception_return.
    result.err().map(u32::from)
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
