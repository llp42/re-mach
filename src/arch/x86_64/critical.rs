// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Critical sections: they hold the current CPU's interrupts back.

use super::per_cpu::PerCpu;
use crate::smp::MachinePerCpu;

/// Enters a critical section on the current CPU; its interrupts wait for
/// the outermost `crit_exit`.
pub fn crit_enter() {
    PerCpu::current().common().enter_critical();
}

/// Leaves a critical section; leaving the outermost runs the interrupts
/// held back.
pub fn crit_exit() {
    let cpu = PerCpu::current();
    if cpu.common().leave_critical() {
        cpu.run_pending_interrupts();
    }
}
