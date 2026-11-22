// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::sync::atomic::AtomicBool;

use intrusive_collections::LinkedList;

use crate::slab::Cache;
use crate::sync::{OnceLock, Token};

use super::processor::Processor;
use super::set::{AllSetsAdapter, ProcessorSet};

/// Every processor set, and how many there are.
pub struct AllSets {
    pub(crate) list: LinkedList<AllSetsAdapter>,
    pub(super) count: u32,
}

/// The processor sets, behind one token: they change only as sets are
/// created and destroyed.
pub static ALL_SETS: Token<AllSets> = Token::new(AllSets {
    list: LinkedList::new(AllSetsAdapter::NEW),
    count: 0,
});

/// The set the kernel creates at boot, where the other CPUs go.
pub static SLAVE_SET: OnceLock<&'static ProcessorSet> = OnceLock::new();

/// The default set, where every processor and thread starts; its own
/// reference never drops.
pub static DEFAULT_SET: ProcessorSet = {
    let mut set = ProcessorSet::new();
    set.active = AtomicBool::new(true);
    set
};

/// The cache processor sets other than the default come from.
pub(super) static SET_CACHE: Cache<ProcessorSet> = Cache::new("processor_set");

/// The boot CPU's processor.
pub static BOOT_PROCESSOR: OnceLock<&'static Processor> = OnceLock::new();
