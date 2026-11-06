// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The bootstrap: tasks for the modules the loader brought.

use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering};

use super::boot_script::{Symbols, Value};
use super::host::HOST;
use super::task;
use crate::arch::{self, Machine as _};
use crate::device;
use crate::ipc::port::Port;

/// Starts the boot modules' tasks; so far it only finds the modules and
/// sets the boot script's variables.
///
/// # Panics
///
/// If the loader brought no module, or a single one with nothing past its
/// command line's first word: compat mode, not yet. If the kernel command
/// line is not UTF-8, or a variable can't be set.
pub fn create() {
    let modules = arch::Current::boot_modules();
    assert!(
        !modules.is_empty(),
        "No bootstrap code loaded with the kernel!"
    );
    if let [module] = modules {
        let line = module.command_line.to_bytes();
        // Nothing but spaces and newlines past the first space, if any.
        let rest = line.splitn(2, |&byte| byte == b' ').nth(1);
        let compat = rest
            .unwrap_or_default()
            .iter()
            .all(|byte| matches!(byte, b' ' | b'\n'));
        assert!(
            !compat,
            "bootstrap_create: a single module in compat mode, not yet"
        );
    }
    let kernel_task = task::KERNEL_TASK
        .get()
        .expect("bootstrap_create: no kernel task");
    let command_line = arch::Current::kernel_command_line()
        .to_str()
        .expect("bootstrap_create: a kernel command line not UTF-8");
    let port = |slot: &AtomicPtr<Port>| {
        Value::Port(NonNull::new(slot.load(Ordering::Acquire)).expect("bootstrap_create: no port"))
    };
    let mut symbols = Symbols::default();
    let mut set = |name: &'static str, value| {
        symbols
            .set(name, value)
            .unwrap_or_else(|error| panic!("cannot set boot-script variable {name}: {error}"));
    };
    set("host-port", port(&HOST.privileged_port));
    set("device-port", port(&device::MASTER_PORT));
    set("kernel-task", port(&kernel_task.self_port));
    set("kernel-command-line", Value::String(command_line));
    // Those the old Hurd's serverboot set.
    let (boot_args, root_device) = compat_strings(command_line);
    set("boot-args", Value::String(boot_args));
    set("root-device", Value::String(root_device));
    // Each FOO=BAR word sets FOO to BAR.
    for word in command_line.split([' ', '\t']) {
        if let Some((name, value)) = word.split_once('=') {
            set(name, Value::String(value));
        }
    }
}

/// The boot flags and the root device of `command_line`, as the old Hurd's
/// serverboot took them: "-x", for no flags, and the last `root=` word's
/// value less any "/dev/", else "UNKNOWN".
///
/// # Panics
///
/// If a word sets boot flags: not yet.
fn compat_strings(command_line: &'static str) -> (&'static str, &'static str) {
    let mut root_device = "UNKNOWN";
    // A word ends at a space or any character below it.
    for word in command_line.split(|character| character <= ' ') {
        assert!(
            !word.starts_with('-'),
            "get_compat_strings: boot flags, not yet"
        );
        if let Some(device) = word.strip_prefix("root=") {
            root_device = device.strip_prefix("/dev/").unwrap_or(device);
        }
    }
    ("-x", root_device)
}
