// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The boot script, which starts the boot modules' tasks; so far its
//! variables, the parse of its lines and their run, with no cleanup.

use core::fmt;
use core::mem::MaybeUninit;
use core::ops::Range;
use core::ptr::NonNull;
use core::slice;

use crate::boot::BootModule;
use crate::ipc::port::Port;
use crate::task::Task;
use crate::{bootstrap, kalloc};

/// A variable's value; only the kinds the bootstrap sets.
#[derive(Clone, Copy)]
pub enum Value {
    /// A string.
    String(#[expect(dead_code, reason = "boot_script_exec's string case reads it")] &'static str),
    /// A port, of which each task the script names it to gets a send right.
    Port(NonNull<Port>),
}

/// A variable, by name.
struct Symbol {
    name: &'static str,
    value: Value,
}

/// The builtin functions' names.
const BUILTINS: [&str; 3] = ["task-create", "task-resume", "prompt-task-resume"];

/// A function to run once every command has run; only the kind met here.
enum Function {
    /// task-resume: resumes the command's task.
    ResumeTask,
}

/// A command: a line's program, and what it runs with.
struct Command {
    /// The module the line is of.
    module: &'static BootModule,
    /// Its program's path.
    path: &'static str,
    /// Its task, once task-create makes it.
    task: Option<&'static Task>,
    /// Its arguments, in the script's.
    arguments: Range<usize>,
    /// Its functions to run once every command has run, in the script's.
    functions: Range<usize>,
}

/// Room for a counted number of `T`, from kalloc once first needed.
struct Table<T: 'static> {
    /// How many it has room for.
    capacity: usize,
    /// Its room, once allocated; empty until then.
    room: &'static mut [MaybeUninit<T>],
    /// How many are added, from the start.
    len: usize,
}

impl<T> Table<T> {
    /// An empty one, with room for `capacity` once allocated.
    const fn new(capacity: usize) -> Self {
        Self {
            capacity,
            room: &mut [],
            len: 0,
        }
    }

    /// Allocates its room, if not yet.
    fn reserve(&mut self) -> Result<(), Error> {
        if self.room.len() < self.capacity {
            let buffer = if size_of::<T>() == 0 {
                NonNull::dangling()
            } else {
                kalloc::allocate(self.capacity * size_of::<T>())
                    .map_err(|_| Error::NoMemory)?
                    .cast()
            };
            // SAFETY: the buffer is the table's alone, aligned and large
            // enough for `capacity` items.
            self.room = unsafe { slice::from_raw_parts_mut(buffer.as_ptr(), self.capacity) };
        }
        Ok(())
    }

    /// Adds `item` after the others, in the room `reserve` allocated.
    const fn push(&mut self, item: T) {
        self.room[self.len].write(item);
        self.len += 1;
    }

    /// The items added, in order.
    const fn items(&self) -> &[T] {
        // SAFETY: the first `len` are written, and a `MaybeUninit<T>` is
        // laid out as a `T`.
        unsafe { slice::from_raw_parts(self.room.as_ptr().cast(), self.len) }
    }
}

impl<T> Drop for Table<T> {
    /// Frees the room `reserve` allocated, if any.
    fn drop(&mut self) {
        let size = size_of_val(self.room);
        // SAFETY: the room is `reserve`'s kalloc buffer of `size` bytes, or
        // of 0, and unused after.
        unsafe { kalloc::free(NonNull::from(&mut *self.room).cast(), size) };
    }
}

/// The boot script: its variables, and the commands its lines give.
pub struct Script {
    /// The variables set, in order.
    symbols: Table<Symbol>,
    /// The commands, a line's each, in order.
    commands: Table<Command>,
    /// The commands' arguments, each command's in a row.
    arguments: Table<Value>,
    /// The commands' functions, each command's in a row.
    functions: Table<Function>,
}

/// What can go wrong in the boot script; only the kinds met here.
pub enum Error {
    /// No memory is left for it.
    NoMemory,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::NoMemory => "no memory",
        })
    }
}

impl Script {
    /// An empty script, with room for `variables` variables and for what
    /// the command lines of `modules` may give: a command each, an argument
    /// for each `${`, a function for each `$(`.
    pub fn new(variables: usize, modules: &[BootModule]) -> Self {
        let count = |pattern: &[u8]| {
            modules
                .iter()
                .map(|module| {
                    let line = module.command_line.to_bytes();
                    line.windows(2).filter(|pair| *pair == pattern).count()
                })
                .sum::<usize>()
        };
        Self {
            symbols: Table::new(variables),
            commands: Table::new(modules.len()),
            arguments: Table::new(count(b"${")),
            functions: Table::new(count(b"$(")),
        }
    }

    /// Adds the variable `name`, of `value`, after any of that name.
    ///
    /// # Errors
    ///
    /// `NoMemory` if the symbol table can't be allocated.
    pub fn set(&mut self, name: &'static str, value: Value) -> Result<(), Error> {
        self.symbols.reserve()?;
        self.symbols.push(Symbol { name, value });
        Ok(())
    }

    /// Parses `line`, the command line of `module`: a program's path, then
    /// `${variable}` arguments and `$(function)` calls. task-create runs
    /// now; task-resume is kept for once every command has run.
    ///
    /// # Errors
    ///
    /// `NoMemory` if the command's room can't be allocated.
    ///
    /// # Panics
    ///
    /// On a comment, a line with no command, plain text, a variable not
    /// set, an assignment, a value in `$(...)`, prompt-task-resume, a
    /// builtin misused or a syntax error: not yet. If task-create fails:
    /// the cleanup, not yet.
    pub fn parse_line(
        &mut self,
        module: &'static BootModule,
        line: &'static str,
    ) -> Result<(), Error> {
        let line = line.trim_start_matches([' ', '\t']);
        assert!(
            !line.starts_with('#'),
            "boot_script_parse_line: a comment, not yet"
        );
        let end = line.find([' ', '\t', '\n']).unwrap_or(line.len());
        assert!(
            end != 0,
            "boot_script_parse_line: a line with no command, not yet"
        );
        let path = &line[..end];
        // Past the character that ends the path.
        let mut rest = line.get(end + 1..).unwrap_or_default();
        // Where GNU Mach allocates the command.
        self.commands.reserve()?;
        self.arguments.reserve()?;
        self.functions.reserve()?;
        let mut command = Command {
            module,
            path,
            task: None,
            arguments: self.arguments.len..self.arguments.len,
            functions: self.functions.len..self.functions.len,
        };
        loop {
            rest = rest.trim_start_matches([' ', '\t']);
            if rest.is_empty() || rest.starts_with('\n') {
                command.arguments.end = self.arguments.len;
                command.functions.end = self.functions.len;
                self.commands.push(command);
                return Ok(());
            }
            // `${...}` names a value, `$(...)` a function.
            let end_char = match rest.get(..2) {
                Some("${") => '}',
                Some("$(") => ')',
                _ => panic!("boot_script_parse_line: plain text, not yet"),
            };
            let symbol = &rest[2..];
            let name = match symbol.find([end_char, '=', '\n']) {
                Some(end) if end != 0 && symbol[end..].starts_with(end_char) => &symbol[..end],
                Some(end) if end != 0 && end_char == ')' && symbol[end..].starts_with('=') => {
                    assert!(
                        !BUILTINS.contains(&&symbol[..end]),
                        "boot_script_parse_line: invalid variable in assignment, not yet"
                    );
                    panic!("boot_script_parse_line: an assignment, not yet")
                }
                _ => panic!("boot_script_parse_line: syntax error, not yet"),
            };
            rest = &symbol[name.len() + 1..];
            match name {
                _ if end_char == '}' && BUILTINS.contains(&name) => {
                    panic!("boot_script_parse_line: invalid variable in expression, not yet")
                }
                "task-create" => command.task = Some(bootstrap::create_task(path)),
                "task-resume" => self.functions.push(Function::ResumeTask),
                "prompt-task-resume" => panic!("boot_script_prompt_task_resume: not yet"),
                _ => {
                    let symbol = self
                        .symbols
                        .items()
                        .iter()
                        .find(|symbol| symbol.name == name)
                        .unwrap_or_else(|| panic!("sym_enter: not yet"));
                    // Alone in `${...}`, a value makes an argument.
                    assert!(
                        end_char == '}',
                        "boot_script_parse_line: a value in $(...), not yet"
                    );
                    self.arguments.push(symbol.value);
                }
            }
        }
    }

    /// Runs the commands: builds each one's command line and arguments from
    /// kalloc, runs it in its task, then frees them; the line holds the
    /// path, then each port argument's name in the task, in decimal, each
    /// ending with a NUL, and the arguments are their offsets in it. Then
    /// runs each command's functions.
    ///
    /// # Errors
    ///
    /// `NoMemory` if a command line or its arguments can't be allocated.
    ///
    /// # Panics
    ///
    /// On a command with no task, a string argument, or a command line
    /// past its room: not yet. As `bootstrap::run_command`, if a command's
    /// thread can't be created or resumed, and `bootstrap::resume_task`, if
    /// its task can't be resumed.
    pub fn run(&self) -> Result<(), Error> {
        for command in self.commands.items() {
            let task = command
                .task
                .unwrap_or_else(|| panic!("boot_script_exec: a command with no task, not yet"));
            // The path and its NUL, with 100 bytes to spare.
            let mut line = Table::new(command.path.len() + 1 + 100);
            line.reserve()?;
            for &byte in command.path.as_bytes() {
                line.push(byte);
            }
            line.push(0);
            let values = &self.arguments.items()[command.arguments.clone()];
            // Each argument's offset in the line, the path's first, with room
            // for GNU Mach's null end.
            let mut arguments = Table::new(values.len() + 2);
            arguments.reserve()?;
            arguments.push(0);
            for value in values {
                let Value::Port(port) = *value else {
                    panic!("boot_script_exec: a string argument, not yet");
                };
                // SAFETY: the script's ports are never freed.
                let name = bootstrap::insert_right(task, unsafe { port.as_ref() });
                // Its name in decimal, written from the end.
                let mut digits = [0; 10];
                let (mut start, mut rest) = (digits.len(), name);
                loop {
                    start -= 1;
                    digits[start] = b'0' + u8::try_from(rest % 10).unwrap();
                    rest /= 10;
                    if rest == 0 {
                        break;
                    }
                }
                let digits = &digits[start..];
                // Room for the digits and a NUL.
                assert!(
                    line.capacity - line.len > digits.len(),
                    "CHECK_CMDLINE_LEN: a longer command line, not yet"
                );
                arguments.push(line.len);
                for &digit in digits {
                    line.push(digit);
                }
                line.push(0);
            }
            bootstrap::run_command(command.module, task, line.items(), arguments.items());
            // The line, then the arguments, as GNU Mach frees them.
            drop(line);
            drop(arguments);
        }
        for command in self.commands.items() {
            for function in &self.functions.items()[command.functions.clone()] {
                match function {
                    Function::ResumeTask => {
                        // The loop above panicked on a command with no task.
                        let task = command
                            .task
                            .expect("boot_script_exec: a command with no task");
                        bootstrap::resume_task(task, command.path);
                    }
                }
            }
        }
        Ok(())
    }
}
