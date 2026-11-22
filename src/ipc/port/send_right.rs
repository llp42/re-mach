// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::mem::ManuallyDrop;
use core::ops::Deref;

use super::port::Port;
use crate::reference::Reference;
/// A send right for a port, released as it drops.
pub struct SendRight(ManuallyDrop<Reference<Port>>);

impl SendRight {
    /// The handle of a send right for `port`, which the caller gives with its
    /// reference.
    pub const fn new(port: Reference<Port>) -> Self {
        Self(ManuallyDrop::new(port))
    }

    /// Its port's reference, which takes the right with it, unreleased.
    pub fn into_reference(self) -> Reference<Port> {
        let mut right = ManuallyDrop::new(self);
        // SAFETY: `right` never drops, and its field is not used after.
        unsafe { ManuallyDrop::take(&mut right.0) }
    }
}

impl AsRef<Reference<Port>> for SendRight {
    fn as_ref(&self) -> &Reference<Port> {
        &self.0
    }
}

impl Deref for SendRight {
    type Target = Port;

    fn deref(&self) -> &Port {
        &self.0
    }
}

impl Drop for SendRight {
    /// Releases the right, as `ipc_port_release_send` does.
    fn drop(&mut self) {
        // SAFETY: drop runs once, and the field is not used after.
        Port::release_send(unsafe { ManuallyDrop::take(&mut self.0) });
    }
}
