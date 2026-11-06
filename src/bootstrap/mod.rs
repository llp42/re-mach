// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The bootstrap: tasks for the modules the loader brought.

mod script;

use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::arch::{self, Machine as _};
use crate::error::Error;
use crate::host::HOST;
use crate::ipc::port::Port;
use crate::ipc::{MessageType, mach_port};
use crate::sched::USER_PRIORITY;
use crate::task::{self, Task};
use crate::{device, print, println};
use script::{Script, Value};

/// Starts the boot modules' tasks; so far it finds the modules, sets the
/// boot script's variables, parses the modules' command lines and runs the
/// script up to each command's arguments.
///
/// # Panics
///
/// If the loader brought no module, or a single one with nothing past its
/// command line's first word: compat mode, not yet. If the kernel command
/// line or a module's is not UTF-8, a variable can't be set, a line can't
/// be parsed, or the script can't run.
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
    // The six variables below, and one per FOO=BAR word.
    let words = command_line
        .split([' ', '\t'])
        .filter(|word| word.contains('='))
        .count();
    let mut script = Script::new(6 + words, modules);
    let mut set = |name: &'static str, value| {
        script
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
    let mut losers = 0;
    for (index, module) in modules.iter().enumerate() {
        let line = module
            .command_line
            .to_str()
            .expect("bootstrap_create: a module command line not UTF-8");
        println!("module {index}: {line}");
        if let Err(error) = script.parse_line(module, line) {
            print!("\n\tERROR: {error}");
            losers += 1;
        }
    }
    println!("{} multiboot modules", modules.len());
    assert!(
        losers == 0,
        "{losers} of {} boot script commands could not be parsed",
        modules.len()
    );
    script
        .run()
        .unwrap_or_else(|error| panic!("ERROR in executing boot script: {error}"));
}

/// The boot script's task-create: a new task for the program at `path`,
/// named after it, its priorities capped at user threads'.
///
/// # Panics
///
/// If the task can't be created: the script's cleanup, not yet.
fn create_task(path: &str) -> &'static Task {
    let task = task::create(None).unwrap_or_else(|error| {
        panic!("boot_script_task_create failed with {error:?}, cleanup not yet")
    });
    task.set_name(path);
    // SAFETY: the host's ports are never freed.
    let host = unsafe { HOST.port.load(Ordering::Acquire).as_ref() }
        .expect("boot_script_task_create: no host port");
    task.set_max_priority(host, USER_PRIORITY);
    task
}

/// The boot script's insert of a port argument: a send right made for
/// `port` and given to `task`; the name it takes there.
fn insert_right(task: &Task, port: &Port) -> u32 {
    port.make_send();
    insert_send_right(task, port)
}

/// Gives `task` a send right for `port` under the first free name from 1;
/// that name.
///
/// # Panics
///
/// If an insert fails but for a name in use.
fn insert_send_right(task: &Task, port: &Port) -> u32 {
    let mut name = 1;
    loop {
        match mach_port::insert_right(&task.space, name, port, MessageType::PortSend) {
            Ok(()) => return name,
            Err(error) => assert!(
                matches!(error, Error::NameExists),
                "task_insert_send_right: {error:?}"
            ),
        }
        name += 1;
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
