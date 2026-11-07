// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The bootstrap: tasks for the modules the loader brought.

mod elf_load;
mod script;

use core::ffi::CStr;
use core::ptr::{self, NonNull};
use core::slice;
use core::sync::atomic::{AtomicPtr, Ordering};

use crate::arch::{self, Machine as _, PAGE_SIZE};
use crate::boot::BootModule;
use crate::error::Error;
use crate::host::HOST;
use crate::ipc::port::Port;
use crate::ipc::{MessageType, mach_port};
use crate::sched::{self, Event, USER_PRIORITY};
use crate::sync::Spinlock;
use crate::task::{self, Task};
use crate::thread::MachineThread as _;
use crate::vm::map::{Change, Map};
use crate::vm::user::Placement;
use crate::vm::{Inheritance, Protection};
use crate::{device, print, println, smp, thread, vm};
use elf_load::{Image, SectionType, Segment};
use script::{Script, Value};

/// Starts the boot modules' tasks: finds the modules, sets the boot
/// script's variables, parses the modules' command lines and runs the
/// script; so far the modules' pages are not freed.
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

/// The boot script's task-resume: resumes `task`, then starts a line for
/// the program at `path`.
///
/// # Panics
///
/// If the task can't be resumed: the script's cleanup, not yet.
fn resume_task(task: &Task, path: &str) {
    task.resume().unwrap_or_else(|error| {
        panic!("boot_script_task_resume failed with {error:?}, cleanup not yet")
    });
    print!("\nstart {path}: ");
}

/// What `run_command` lends the thread it makes, on its own stack: the
/// module, the command line and its arguments, and whether the thread is
/// done with them.
struct LoaderInfo<'a> {
    /// The module whose program the thread loads.
    module: &'static BootModule,
    /// The command line: the path, then each argument, each ending with a
    /// NUL.
    line: &'a [u8],
    /// Where each argument starts in `line`, the path first.
    arguments: &'a [usize],
    /// Whether the thread is done with them, behind the lock `run_command`
    /// sleeps with.
    done: Spinlock<bool>,
}

/// The boot script's run of a command: a new thread in `task`, which loads
/// the program in `module` and starts it with `arguments`, their offsets in
/// `line`; it waits until the thread is done with them, then ends the
/// line.
///
/// # Panics
///
/// If the thread can't be created or resumed.
fn run_command(module: &'static BootModule, task: &'static Task, line: &[u8], arguments: &[usize]) {
    let thread = thread::create(task)
        .unwrap_or_else(|error| panic!("boot_script_exec_cmd: thread_create: {error:?}"));
    let info = LoaderInfo {
        module,
        line,
        arguments,
        done: Spinlock::new(false),
    };
    let mut done = info.done.lock();
    thread
        .argument
        .store(ptr::from_ref(&info).cast_mut().cast(), Ordering::Release);
    thread.start(load_program);
    thread
        .resume()
        .unwrap_or_else(|error| panic!("boot_script_exec_cmd: thread_resume: {error:?}"));
    // The thread refers to the info until done.
    while !*done {
        sched::sleep(Event::of(&info), done);
        done = info.done.lock();
    }
    drop(done);
    print!("\n");
}

/// The work of a thread `run_command` makes, in its boot task: loads the
/// module's program into the task and starts it; so far it loads the
/// program, builds its stack and arguments, prints so, suspends the task,
/// tells `run_command` it is done and returns to user mode.
///
/// # Panics
///
/// If it was given no info, or the program can't be loaded; as
/// `Task::suspend`.
fn load_program() -> ! {
    let thread = smp::current()
        .active_thread()
        .expect("user_bootstrap: no current thread");
    // SAFETY: `run_command` gave it a `LoaderInfo`, which it keeps until the
    // thread is done with it.
    let info = unsafe {
        thread
            .argument
            .load(Ordering::Acquire)
            .cast::<LoaderInfo<'_>>()
            .as_ref()
    }
    .expect("user_bootstrap: no info");
    let fail = |error: elf_load::Error| -> ! {
        let path = CStr::from_bytes_until_nul(&info.line[info.arguments[0]..])
            .expect("user_bootstrap: a path with no NUL")
            .to_str()
            .expect("user_bootstrap: a path not UTF-8");
        panic!(
            "Cannot load user executable module (error code {}): {}",
            i32::from(error),
            path
        )
    };
    let range = &info.module.range;
    // SAFETY: the module's copy in boot memory, unwritten and kept until the
    // bootstrap frees it.
    let bytes =
        unsafe { slice::from_raw_parts(range.start.direct_map_ptr(), range.end - range.start) };
    let image = elf_load::parse(bytes).unwrap_or_else(|error| fail(error));
    for segment in image.segments() {
        load_segment(&thread.task.map, bytes, &segment).unwrap_or_else(|error| fail(error));
    }
    print!("task loaded:");
    // GNU Mach's argv.
    let argv = info.arguments.iter().map(|&offset| {
        CStr::from_bytes_until_nul(&info.line[offset..])
            .expect("user_bootstrap: an argument with no NUL")
    });
    build_arguments_and_stack(&thread.task.map, &image, argv.clone());
    for argument in argv {
        let argument = argument
            .to_str()
            .expect("user_bootstrap: an argument not UTF-8");
        print!(" {argument}");
    }
    thread.task.suspend();
    // `run_command` may drop the info once it is done, so the wakeup takes
    // its address alone.
    let event = Event::of(info);
    let mut done = info.done.lock();
    assert!(!*done);
    *done = true;
    drop(done);
    sched::wakeup(event);
    arch::Pcb::return_to_user()
}

/// Maps the current task's stack into `map`, as `image` asks, and copies
/// `argv` to its top: the argument count, a pointer to each argument, a
/// null end, an empty environment's null end, then the strings; the
/// current thread's first user registers start the program there.
fn build_arguments_and_stack<'a>(
    map: &Map,
    image: &Image<'_>,
    argv: impl Iterator<Item = &'a CStr> + Clone,
) {
    /// The stack's bytes.
    const STACK_SIZE: usize = 2 * 64 * 1024;
    /// A slot's bytes: GNU Mach's `rpc_vm_offset_t`.
    const SLOT_SIZE: usize = size_of::<usize>();
    let (count, strings) = argv.clone().fold((0_usize, 0), |(count, bytes), argument| {
        (count + 1, bytes + argument.count_bytes() + 1)
    });
    // The count, the pointers and their end, then the environment's end.
    let slots = SLOT_SIZE + (count + 1 + 1) * SLOT_SIZE;
    let stack_size = STACK_SIZE.next_multiple_of(PAGE_SIZE);
    let stack_base = arch::Pcb::user_stack_low(stack_size);
    // Here and below, results go unchecked, as GNU Mach's do.
    let _ = vm::user::map(
        map,
        stack_base,
        stack_size,
        0,
        Placement::Fixed,
        None,
        image.stack_protection,
        Protection::all(),
        Inheritance::Copy,
    );
    let mut slot = arch::Pcb::set_user_registers(
        stack_base..stack_base + stack_size,
        image.entry,
        strings + slots,
    );
    let mut string = slot + slots;
    let _ = arch::Current::copy_out(&count.to_ne_bytes(), slot);
    slot += SLOT_SIZE;
    for argument in argv {
        let _ = arch::Current::copy_out(&string.to_ne_bytes(), slot);
        slot += SLOT_SIZE;
        let bytes = argument.to_bytes_with_nul();
        let _ = arch::Current::copy_out(bytes, string);
        string += bytes.len();
    }
    let _ = arch::Current::copy_out(&0_usize.to_ne_bytes(), slot);
    slot += SLOT_SIZE;
    let _ = arch::Current::copy_out(&0_usize.to_ne_bytes(), slot);
}

/// Loads `segment` of the image in `bytes` into `map`, the current task's:
/// checks the segment, allocates its pages, copies its file bytes in and
/// protects its pages as it asks.
///
/// # Errors
///
/// `OutOfBounds` if the segment's file bytes lie past the image's end.
///
/// # Panics
///
/// If the segment takes no memory, less than its file bytes, or pages past
/// the address space's end, if its pages can't be allocated where it asks
/// or protected, or if its file bytes can't be copied in.
fn load_segment(map: &Map, bytes: &[u8], segment: &Segment) -> Result<(), elf_load::Error> {
    if segment
        .offset
        .checked_add(segment.file_size)
        .is_none_or(|end| end > bytes.len())
    {
        return Err(elf_load::Error::OutOfBounds);
    }
    if !segment.section_type.contains(SectionType::ALLOCATE) {
        return Ok(());
    }
    assert!(segment.memory_size > 0);
    assert!(segment.memory_size >= segment.file_size);
    let end = segment
        .address
        .checked_add(segment.memory_size)
        .and_then(|end| end.checked_next_multiple_of(PAGE_SIZE))
        .expect("read_exec: a segment past the address space's end");
    let pages = segment.address & !(PAGE_SIZE - 1)..end;
    let start = vm::user::allocate(map, pages.start, pages.end - pages.start, Placement::Fixed)
        .unwrap_or_else(|error| panic!("read_exec: vm_allocate: {error:?}"));
    assert_eq!(start, pages.start);
    if segment.file_size > 0 {
        let file = &bytes[segment.offset..segment.offset + segment.file_size];
        arch::Current::copy_out(file, segment.address)
            .unwrap_or_else(|error| panic!("read_exec: copyout: {error:?}"));
    }
    let protection = Protection::from_bits_truncate(segment.section_type.bits());
    if protection != Protection::all() {
        vm::user::protect(
            map,
            pages.start,
            pages.end - pages.start,
            Change::Current,
            protection,
        )
        .unwrap_or_else(|error| panic!("read_exec: vm_protect: {error:?}"));
    }
    Ok(())
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
