// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Rights: what a space's entries hold of ports.

use core::ptr::{self, NonNull};

use super::MessageType;
use super::entry::{Entry, Rights};
use super::port::{Port, PortState};
use super::space::{Space, SpaceState};
use crate::arch::PhysAddr;
use crate::sync::SpinlockGuard;

/// `port`'s key in a reverse map: its physical address over 8, as ports
/// are in the direct map.
fn key(port: &Port) -> u64 {
    u64::try_from(PhysAddr::from_direct_map(ptr::from_ref(port)).0 >> 3).unwrap()
}

/// Looks for a send or receive right for `port` in `space`, of state
/// `state`, as `ipc_right_reverse` does; finds none so far.
///
/// # Panics
///
/// If `port` is alive and the space holds its receive right or a send
/// right for it: not yet.
pub(super) fn reverse(space: &Space, state: &SpaceState, port: &Port) {
    let port_state = port.state.lock();
    // A dead port has none.
    if port_state.active {
        assert!(
            port_state.receiver != NonNull::from(space),
            "ipc_right_reverse: the space's receive right, not yet"
        );
        assert!(
            state.reverse_map.lookup(key(port)).is_none(),
            "ipc_right_reverse: a send right, not yet"
        );
    }
    drop(port_state);
}

/// Puts the right `message_type` names, for `port`, of state `port_state`,
/// in `entry`, `name` in the space of state `state`; the entry takes the
/// right and its reference, and the port is unlocked.
///
/// # Safety
///
/// `entry` is the space's entry for `name`, as its map holds it, and no
/// reference to it is alive.
///
/// # Panics
///
/// If `entry` holds rights: not yet.
pub(super) unsafe fn copy_out(
    state: &mut SpaceState,
    name: u32,
    entry: NonNull<Entry>,
    message_type: MessageType,
    port: &Port,
    port_state: SpinlockGuard<'_, PortState>,
) {
    match message_type {
        MessageType::PortSend => {
            // SAFETY: the caller's; the space's lock, which `state` stands
            // for, guards it.
            let used = unsafe { entry.as_ref() }.in_use();
            assert!(!used, "ipc_right_copyout: an entry with rights, not yet");
            drop(port_state);
            // As in GNU Mach, a failure leaves the entry out of the reverse map.
            let _ = state.reverse_map.insert(key(port), entry);
            // SAFETY: as above; this reference is the only one, and dies
            // before the maps' pointers are used again.
            let entry = unsafe { &mut *entry.as_ptr() };
            entry.name = name;
            entry.bits.rights |= Rights::SEND;
            entry.bits.user_references += 1;
        }
    }
}
