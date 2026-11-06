// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The boot script, which starts the boot modules' tasks; so far only its
//! variables.

use core::fmt;
use core::mem::MaybeUninit;
use core::ptr::NonNull;
use core::slice;

use super::kalloc;
use crate::ipc::port::Port;

/// A variable's value; only the kinds the bootstrap sets.
pub enum Value {
    /// A string.
    String(#[expect(dead_code, reason = "boot_script_parse_line reads it")] &'static str),
    /// A port, of which each task the script names it to gets a send right.
    Port(#[expect(dead_code, reason = "boot_script_exec reads it")] NonNull<Port>),
}

/// A variable, by name.
#[expect(dead_code, reason = "boot_script_parse_line reads them")]
struct Symbol {
    name: &'static str,
    value: Value,
}

/// How many symbols the table grows by.
const GROWTH: usize = 20;

/// The symbol table: the variables set, in order.
#[derive(Default)]
pub struct Symbols {
    /// Room for the symbols, from kalloc, once the first is set.
    table: Option<&'static mut [MaybeUninit<Symbol>]>,
    /// How many are set, from the table's start.
    len: usize,
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

impl Symbols {
    /// Adds the variable `name`, of `value`, after any of that name.
    ///
    /// # Errors
    ///
    /// `NoMemory` if the table can't be allocated.
    ///
    /// # Panics
    ///
    /// If the table is full: growing it, not yet.
    pub fn set(&mut self, name: &'static str, value: Value) -> Result<(), Error> {
        let Self { table, len } = self;
        let table = if let Some(table) = table {
            table
        } else {
            let buffer =
                kalloc::allocate(GROWTH * size_of::<Symbol>()).map_err(|_| Error::NoMemory)?;
            // SAFETY: the buffer is the table's alone, aligned and large
            // enough for `GROWTH` symbols.
            table.insert(unsafe { slice::from_raw_parts_mut(buffer.cast().as_ptr(), GROWTH) })
        };
        table
            .get_mut(*len)
            .expect("boot_script_set_variable: a full symbol table, not yet")
            .write(Symbol { name, value });
        *len += 1;
        Ok(())
    }
}
