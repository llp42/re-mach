// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The boot script, which starts the boot modules' tasks; so far its
//! variables, the parse of its lines, their run and its cleanup.

use core::fmt;
use core::mem::{ManuallyDrop, MaybeUninit};
use core::ops::Range;
use core::ptr::NonNull;
use core::slice;

use crate::boot::BootModule;
use crate::ipc::port::port::Port;
use crate::ipc::port::port_name::PortName;
use crate::reference::Reference;
use crate::task::task::Task;
use crate::{bootstrap, kalloc};

/// A variable's value; only the kinds the bootstrap sets and the script
/// gives.
#[derive(Clone, Copy)]
pub enum Value {
    /// A string.
    String(&'static str),
    /// A port, of which each task the script names it to gets a send right.
    Port(NonNull<Port>),
    /// A task, whose port each task the script names it to gets a send
    /// right for.
    Task(&'static Task),
    /// A symbol whose value is resolved when the script runs.
    Symbol(usize),
}

/// A variable, by name, and its value once assigned.
struct Symbol {
    name: &'static str,
    value: Option<Value>,
}

/// The builtin functions' names.
const BUILTINS: [&str; 3] = ["task-create", "task-resume", "prompt-task-resume"];

/// A function to run once every command has run; only the kind met here.
enum Function {
    /// task-resume: resumes the command's task.
    ResumeTask,
}

/// An argument: the text before a `${...}`, then the value the run
/// appends to it.
struct Argument {
    /// Its text, if it has any.
    text: Option<&'static str>,
    /// Its value, if it has one.
    value: Option<Value>,
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

    /// The item added at `index`, mutably.
    fn get_mut(&mut self, index: usize) -> &mut T {
        // SAFETY: the first `len` are written, and `index` is below `len`.
        unsafe { &mut *self.room.as_mut_ptr().cast::<T>().add(index) }
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

/// The boot script: its variables, and the commands its lines give. Its
/// tables drop in cleanup's order.
pub struct Script {
    /// The commands' arguments, each command's in a row.
    arguments: Table<Argument>,
    /// The commands' functions, each command's in a row.
    functions: Table<Function>,
    /// The commands, a line's each, in order.
    commands: Table<Command>,
    /// The variables set, in order.
    symbols: Table<Symbol>,
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
    /// for each word, a variable for each `${` or `$(`, a function for each
    /// `$(`.
    pub fn new(variables: usize, modules: &[BootModule]) -> Self {
        let arguments = modules
            .iter()
            .map(|module| {
                module
                    .command_line
                    .to_bytes()
                    .split(|&byte| matches!(byte, b' ' | b'\t' | b'\n'))
                    .filter(|token| !token.is_empty())
                    .count()
            })
            .sum::<usize>();
        let count = |pattern: &[u8]| {
            modules
                .iter()
                .map(|module| {
                    let line = module.command_line.to_bytes();
                    line.windows(2).filter(|pair| *pair == pattern).count()
                })
                .sum::<usize>()
        };
        let symbols = variables + count(b"${") + count(b"$(");
        Self {
            symbols: Table::new(symbols),
            commands: Table::new(modules.len()),
            arguments: Table::new(arguments),
            functions: Table::new(count(b"$(")),
        }
    }

    /// Sets the variable `name` to `value`, adding it if no line named it
    /// yet, as `boot_script_set_variable` does.
    ///
    /// # Errors
    ///
    /// `NoMemory` if the symbol table can't be allocated.
    pub fn set(&mut self, name: &'static str, value: Value) -> Result<(), Error> {
        let index = self.symbol(name)?;
        self.symbols.get_mut(index).value = Some(value);
        Ok(())
    }

    /// The index of the variable `name`, adding it unset if no line named
    /// it yet, as `sym_enter` does.
    ///
    /// # Errors
    ///
    /// `NoMemory` if the symbol table can't be allocated.
    fn symbol(&mut self, name: &'static str) -> Result<usize, Error> {
        if let Some(index) = self
            .symbols
            .items()
            .iter()
            .position(|symbol| symbol.name == name)
        {
            return Ok(index);
        }
        self.symbols.reserve()?;
        let index = self.symbols.len;
        self.symbols.push(Symbol { name, value: None });
        Ok(index)
    }

    /// Parses `line`, the command line of `module`: a program's path, then
    /// its arguments, text and `${variable}`s, and `$(function)` calls.
    /// task-create runs now; task-resume is kept for once every command
    /// has run.
    ///
    /// # Errors
    ///
    /// `NoMemory` if the command's room can't be allocated.
    ///
    /// # Panics
    ///
    /// On a syntax error, a variable not set, an assignment, a value in
    /// `$(...)`, a builtin in `${...}`, or prompt-task-resume: not yet.
    /// If task-create fails: the cleanup, not yet.
    pub fn parse_line(
        &mut self,
        module: &'static BootModule,
        line: &'static str,
    ) -> Result<(), Error> {
        let line = line.trim_start_matches([' ', '\t']);
        if line.is_empty() || line.starts_with('\n') || line.starts_with('#') {
            return Ok(());
        }
        let end = line.find([' ', '\t', '\n']).unwrap_or(line.len());
        let path = &line[..end];
        let mut rest = line.get(end..).unwrap_or_default();
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
            if rest.starts_with("${") || rest.starts_with("$(") {
                let (argument, following) = self.parse_expression(rest, &mut command)?;
                if let Some(argument) = argument {
                    self.arguments.push(argument);
                }
                rest = following;
                continue;
            }
            // Text, up to a space or a `${...}` in the same argument.
            let stop = rest.find([' ', '\t', '\n']).unwrap_or(rest.len());
            if let Some(start) = rest[..stop].find("${") {
                let (value, following) = self.parse_value(&rest[start..])?;
                self.arguments.push(Argument {
                    text: Some(&rest[..start]),
                    value: Some(value),
                });
                rest = following;
            } else {
                self.arguments.push(Argument {
                    text: Some(&rest[..stop]),
                    value: None,
                });
                rest = &rest[stop..];
            }
        }
    }

    /// Parses the `${...}` or `$(...)` `rest` starts with: gives the
    /// argument it makes, if any, and what follows.
    ///
    /// # Errors
    ///
    /// `NoMemory` if a variable's room can't be allocated.
    ///
    /// # Panics
    ///
    /// On a syntax error, a builtin in `${...}`, a malformed assignment, a
    /// value in `$(...)`, or prompt-task-resume: not yet.
    fn parse_expression(
        &mut self,
        rest: &'static str,
        command: &mut Command,
    ) -> Result<(Option<Argument>, &'static str), Error> {
        if rest.starts_with("${") {
            let (value, rest) = self.parse_value(rest)?;
            return Ok((
                Some(Argument {
                    text: None,
                    value: Some(value),
                }),
                rest,
            ));
        }
        let end = rest
            .find(')')
            .unwrap_or_else(|| panic!("boot_script_parse_line: syntax error"));
        let expression = &rest[2..end];
        if let Some((target, function)) = expression.split_once('=') {
            assert!(
                !target.is_empty() && !BUILTINS.contains(&target),
                "boot_script_parse_line: invalid variable in assignment"
            );
            match function {
                "task-create" => {
                    let task = bootstrap::create_task(command.path);
                    command.task = Some(task);
                    let symbol = self.symbol(target)?;
                    self.symbols.get_mut(symbol).value = Some(Value::Task(task));
                }
                _ => panic!("boot_script_parse_line: a function in assignment, not yet"),
            }
        } else {
            match expression {
                "task-create" => command.task = Some(bootstrap::create_task(command.path)),
                "task-resume" => self.functions.push(Function::ResumeTask),
                "prompt-task-resume" => panic!("boot_script_prompt_task_resume: not yet"),
                _ => panic!("boot_script_parse_line: a value in $(...), not yet"),
            }
        }
        Ok((None, &rest[end + 1..]))
    }

    /// Parses the `${...}` `rest` starts with: gives its value and what
    /// follows, adding the variable if no line named it yet.
    ///
    /// # Errors
    ///
    /// `NoMemory` if the symbol table can't be allocated.
    ///
    /// # Panics
    ///
    /// On a syntax error, or a builtin in `${...}`.
    fn parse_value(&mut self, rest: &'static str) -> Result<(Value, &'static str), Error> {
        let end = rest
            .find('}')
            .unwrap_or_else(|| panic!("boot_script_parse_line: syntax error"));
        let name = &rest[2..end];
        assert!(
            !name.is_empty() && !name.contains('=') && !BUILTINS.contains(&name),
            "boot_script_parse_line: invalid variable in expression"
        );
        let symbol = self.symbol(name)?;
        Ok((Value::Symbol(symbol), &rest[end + 1..]))
    }

    /// Runs the commands: builds each one's command line and arguments from
    /// kalloc, runs it in its task, then frees them; the line holds the
    /// path, then each argument's text and value, each ending with a NUL,
    /// and the arguments are their offsets in it. Then runs each command's
    /// functions, and drops the script.
    ///
    /// # Errors
    ///
    /// `NoMemory` if a command line or its arguments can't be allocated.
    ///
    /// # Panics
    ///
    /// On a command with no task, or a variable never assigned. As
    /// `bootstrap::run_command`, if a command's thread can't be created or
    /// resumed, and `bootstrap::resume_task`, if its task can't be resumed.
    pub fn run(self) -> Result<(), Error> {
        for command in self.commands.items() {
            let task = command
                .task
                .unwrap_or_else(|| panic!("boot_script_exec: a command with no task, not yet"));
            let values = &self.arguments.items()[command.arguments.clone()];
            // The path, then each argument's text and value, with its NUL.
            let mut capacity = command.path.len() + 1;
            for argument in values {
                capacity += argument.text.map_or(0, str::len);
                if let Some(value) = argument.value {
                    capacity += value_length(&self.symbols, value);
                }
                capacity += 1;
            }
            let mut line = Table::new(capacity);
            line.reserve()?;
            for &byte in command.path.as_bytes() {
                line.push(byte);
            }
            line.push(0);
            // Each argument's offset in the line, the path's first, with room
            // for GNU Mach's null end.
            let mut arguments = Table::new(values.len() + 2);
            arguments.reserve()?;
            arguments.push(0);
            for argument in values {
                arguments.push(line.len);
                if let Some(text) = argument.text {
                    for &byte in text.as_bytes() {
                        line.push(byte);
                    }
                }
                if let Some(value) = argument.value {
                    match resolve(&self.symbols, value) {
                        Value::String(text) => {
                            for &byte in text.as_bytes() {
                                line.push(byte);
                            }
                        }
                        Value::Port(port) => {
                            // SAFETY: the slots the script's ports come from keep their
                            // references for good, which the handle borrows and never drops.
                            let port = ManuallyDrop::new(unsafe { Reference::from_raw(port) });
                            push_name(&mut line, bootstrap::insert_right(task, &port));
                        }
                        Value::Task(target) => {
                            push_name(&mut line, bootstrap::insert_task_right(task, target));
                        }
                        Value::Symbol(_) => unreachable!(),
                    }
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

/// Resolves `value` through the variables it names, as `boot_script_exec`
/// does.
///
/// # Panics
///
/// If a variable is never assigned.
fn resolve(symbols: &Table<Symbol>, value: Value) -> Value {
    let mut value = value;
    while let Value::Symbol(index) = value {
        let symbol = &symbols.items()[index];
        value = symbol
            .value
            .unwrap_or_else(|| panic!("boot_script_exec: undefined symbol {}", symbol.name));
    }
    value
}

/// How many bytes `value` takes in a command line, once resolved.
fn value_length(symbols: &Table<Symbol>, value: Value) -> usize {
    match resolve(symbols, value) {
        Value::String(text) => text.len(),
        Value::Port(_) | Value::Task(_) => 10,
        Value::Symbol(_) => unreachable!(),
    }
}

/// Pushes `name`'s decimal digits to `line`.
fn push_name(line: &mut Table<u8>, name: PortName) {
    // Its name in decimal, written from the end.
    let mut digits = [0; 10];
    let (mut start, mut rest) = (digits.len(), u32::from(name));
    loop {
        start -= 1;
        digits[start] = b'0' + u8::try_from(rest % 10).unwrap();
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    for &digit in &digits[start..] {
        line.push(digit);
    }
}

impl Drop for Script {
    /// Drops each command's reference on its task; the tables follow, as
    /// GNU Mach's cleanup frees them.
    fn drop(&mut self) {
        for command in self.commands.items() {
            if let Some(task) = command.task {
                // SAFETY: the reference task-create gave the command, dropped
                // here.
                drop(unsafe { Reference::from_raw(NonNull::from(task)) });
            }
        }
    }
}
