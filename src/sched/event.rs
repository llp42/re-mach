// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::num::{NonZero, TryFromIntError};
use core::ptr::NonNull;

use crate::sched::queue::SLEEP_QUEUES;

/// What a thread waits for: an address, never 0.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Event(NonZero<usize>);

impl Event {
    /// The event of `object`'s address.
    pub fn of<T>(object: &T) -> Self {
        Self(NonNull::from(object).addr())
    }

    /// Its sleep queue: its address as a signed number, complemented if
    /// negative, modulo the queues.
    pub(super) const fn hash(self) -> usize {
        let address = self.0.get().cast_signed();
        let positive = if address < 0 { !address } else { address };
        positive.cast_unsigned() % SLEEP_QUEUES
    }
}

impl From<Event> for usize {
    fn from(event: Event) -> Self {
        event.0.get()
    }
}

/// Fails for 0, which no event is.
impl TryFrom<usize> for Event {
    type Error = TryFromIntError;

    fn try_from(address: usize) -> Result<Self, TryFromIntError> {
        NonZero::try_from(address).map(Self)
    }
}
