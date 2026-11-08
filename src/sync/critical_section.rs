// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Critical sections: they hold the current CPU's interrupts back.

use core::marker::PhantomData;

use crate::arch;
use crate::smp::{self, MachinePerCpu};

/// A critical section on the current CPU: its interrupts wait until the
/// outermost one ends, and run then. Entering returns it; dropping it
/// leaves.
pub struct CriticalSection {
    /// It stays on the CPU that entered it.
    _cpu: PhantomData<*const ()>,
}

impl CriticalSection {
    /// Enters a critical section on the current CPU.
    #[must_use = "dropping it leaves the critical section"]
    pub fn enter() -> Self {
        crit_enter();
        Self { _cpu: PhantomData }
    }

    /// The critical section this CPU is in that no guard holds, such as
    /// the one a switch to a new thread was made in.
    ///
    /// # Safety
    ///
    /// The CPU is in such a section, and only this guard leaves it.
    #[must_use = "dropping it leaves the critical section"]
    pub const unsafe fn inherit() -> Self {
        Self { _cpu: PhantomData }
    }
}

impl Drop for CriticalSection {
    fn drop(&mut self) {
        crit_exit();
    }
}

/// Raises the current CPU's critical nesting.
pub(super) fn crit_enter() {
    smp::current().enter_critical();
}

/// Lowers it; leaving the outermost runs the interrupts held back, unless
/// in an interrupt handler, where they wait for the context it interrupted.
pub(super) fn crit_exit() {
    let cpu = arch::PerCpu::current();
    if cpu.common().leave_critical() && !cpu.common().in_interrupt() {
        cpu.run_pending_interrupts();
    }
}
