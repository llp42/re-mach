// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use crate::vm::VirtualAddress;

/// What `Map::copy_in` does with the range it copies.
#[derive(Clone, Copy)]
pub enum Source {
    /// Leaves it in the map, sharing its objects with the copy.
    Keep,
    /// Removes it from the map.
    Destroy,
}

/// Whether `Map::pageable` checks its range.
#[derive(Clone, Copy)]
pub enum RangeCheck {
    /// It holds no hole, and each entry allows the access.
    Checked,
    /// It is taken as it is.
    Unchecked,
}

/// Which of its protections `Map::protect` sets on each entry.
#[derive(Clone, Copy)]
pub enum Change {
    /// What it allows now.
    Current,
    /// The most it may allow, which also cuts what it allows now.
    Maximum,
}

/// Where `Map::enter` puts what it maps.
#[derive(Clone, Copy)]
pub enum Placement {
    /// From the address given.
    Fixed(VirtualAddress),
    /// Wherever it fits.
    Anywhere,
}

/// What an entry `Map::enter_anywhere` makes is for.
#[derive(Clone, Copy)]
pub enum Usage {
    /// Memory, wired throughout.
    Memory,
    /// A kernel stack, above a guard page that is never wired.
    KernelStack,
}
