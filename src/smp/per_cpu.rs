// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
//! Per-CPU data: the machine-independent part, and the trait through which
//! it reaches the machine's part.

use core::sync::atomic::{AtomicU32, Ordering, compiler_fence};

use crate::arch;
use crate::kernel::sched::{SLEEP_QUEUES, SleepQueue};
use crate::kernel::thread::SleepQueueAdapter;

/// One CPU's machine-independent data, held in the machine's part.
pub struct PerCpu {
    id: usize,
    /// How deep its interrupt handlers nest.
    interrupt_nesting: AtomicU32,
    /// How deep its critical sections nest, until threads hold the count.
    critical_nesting: AtomicU32,
    /// Its sleep queues, by event hash; only it touches them, inside a
    /// critical section.
    #[expect(dead_code, reason = "assert_wait and thread_wakeup use them")]
    sleep_queues: [SleepQueue; SLEEP_QUEUES],
}

impl PerCpu {
    /// CPU `id`'s, before it starts.
    #[expect(
        clippy::large_stack_arrays,
        reason = "only PER_CPU's initializer calls it, at compile time"
    )]
    pub const fn new(id: usize) -> Self {
        Self {
            id,
            interrupt_nesting: AtomicU32::new(0),
            critical_nesting: AtomicU32::new(0),
            sleep_queues: [const { SleepQueue::new(SleepQueueAdapter::NEW) }; SLEEP_QUEUES],
        }
    }

    /// Its number.
    pub const fn id(&self) -> usize {
        self.id
    }

    /// Enters an interrupt handler on this CPU; dropping the guard leaves it.
    pub fn enter_interrupt(&self) -> InterruptGuard<'_> {
        raise(&self.interrupt_nesting);
        InterruptGuard(self)
    }

    /// Enters a critical section on this CPU.
    pub fn enter_critical(&self) {
        raise(&self.critical_nesting);
        compiler_fence(Ordering::SeqCst);
    }

    /// Leaves a critical section on this CPU, returning whether it was the
    /// outermost.
    pub fn leave_critical(&self) -> bool {
        compiler_fence(Ordering::SeqCst);
        lower(&self.critical_nesting) == 0
    }

    /// Whether this CPU is in a critical section.
    pub fn in_critical_section(&self) -> bool {
        self.critical_nesting.load(Ordering::Relaxed) != 0
    }
}

/// An interrupt handler running on its CPU; dropping it leaves the handler.
pub struct InterruptGuard<'a>(&'a PerCpu);

impl Drop for InterruptGuard<'_> {
    fn drop(&mut self) {
        lower(&self.0.interrupt_nesting);
    }
}

/// Raises a count only its own CPU changes; an interrupt between the load
/// and the store leaves it as it found it.
fn raise(count: &AtomicU32) {
    count.store(count.load(Ordering::Relaxed) + 1, Ordering::Relaxed);
}

/// Lowers such a count, returning the new value.
fn lower(count: &AtomicU32) -> u32 {
    let lowered = count.load(Ordering::Relaxed) - 1;
    count.store(lowered, Ordering::Relaxed);
    lowered
}

/// The machine's part of each CPU's data, holding a `PerCpu`; the
/// machine-independent side reaches it only through this.
pub trait MachinePerCpu: Sync + 'static {
    /// The current CPU's.
    fn current() -> &'static Self;

    /// Its machine-independent part.
    fn common(&self) -> &PerCpu;

    /// Runs the interrupts its critical sections held back.
    fn run_pending_interrupts(&self);
}

/// The current CPU's machine-independent data.
pub fn current() -> &'static PerCpu {
    arch::PerCpu::current().common()
}
