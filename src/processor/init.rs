// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::sync::atomic::Ordering;

use crate::config::MAX_CPUS;
use crate::smp;
use crate::{ipc_host, machine};

use super::all_sets::{BOOT_PROCESSOR, DEFAULT_SET, SET_CACHE, SLAVE_SET};
use super::set::ProcessorSet;
use super::state::State;

/// Records the boot CPU's processor, on the boot CPU.
///
/// # Panics
///
/// On a second call.
pub fn bootstrap() {
    assert!(
        BOOT_PROCESSOR.set(smp::current().processor()).is_ok(),
        "processor: bootstrapped twice"
    );
}

/// Sets up processor sets, on the boot CPU: gives every processor but the
/// boot CPU's its ports, and creates the slave set.
pub fn init() {
    SET_CACHE.register();
    for cpu in (1..MAX_CPUS).filter(|&cpu| machine::has_processor(cpu)) {
        ipc_host::init_processor(smp::of(cpu).processor());
    }
    DEFAULT_SET.join_all();
    let set = ProcessorSet::create().unwrap_or_else(|error| panic!("pset_sys_init: {error:?}"));
    assert!(SLAVE_SET.set(set).is_ok(), "pset_sys_init: twice");
}

/// Brings the current CPU's processor up, on that CPU, under the default
/// set's token and the slave set's: it joins the default set, or the slave
/// set for a CPU but the boot CPU's, and runs.
pub fn up(cpu: usize) {
    let processor = smp::current().processor();
    let slave_set = SLAVE_SET.get().expect("cpu_up: no slave set");
    let default_members = DEFAULT_SET.members.lock();
    let slave_members = slave_set.members.lock();
    let set = if cpu == 0 { &DEFAULT_SET } else { slave_set };
    set.add_processor(processor);
    processor
        .state
        .store(State::Running.number(), Ordering::Release);
    drop(slave_members);
    drop(default_members);
}
