// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Collections that need no global allocator.

mod array_vec;
pub mod radix_tree;

pub use array_vec::ArrayVec;
