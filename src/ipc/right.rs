// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Rights: what a space's entries hold of ports.

use core::ptr::{self, NonNull};

use super::MessageType;
use super::entry::Rights;
use super::port::{Port, PortName, PortState};
use super::space::{Space, SpaceState};
use crate::arch::PhysAddr;
use crate::sync::SpinlockGuard;

/// `port`'s key in a reverse map: its physical address over 8, as ports
/// are in the direct map.
fn key(port: &Port) -> u64 {
    u64::try_from(PhysAddr::from_direct_map(ptr::from_ref(port)).0 >> 3).unwrap()
}

/// Looks for a send or receive right for `port` in `space`, of state
/// `state`, as `ipc_right_reverse` does: gives a send right's name, with
/// the port locked.
///
/// # Panics
///
/// If `port` is alive and the space holds its receive right: not yet.
pub(super) fn reverse<'a>(
    space: &Space,
    state: &SpaceState,
    port: &'a Port,
) -> Option<(PortName, SpinlockGuard<'a, PortState>)> {
    let port_state = port.state.lock();
    // A dead port has none.
    if port_state.active {
        assert!(
            port_state.receiver != NonNull::from(space),
            "ipc_right_reverse: the space's receive right, not yet"
        );
        if let Some(entry) = state.reverse_map.lookup(key(port)) {
            // SAFETY: the reverse map holds copies of the name map's pointers,
            // to entries it still holds, and `state` keeps the name map shared,
            // so no exclusive lend is alive.
            let name = unsafe { entry.as_ref() }.name;
            return Some((name, port_state));
        }
    }
    drop(port_state);
    None
}

/// Puts the right `message_type` names, for `port`, of state `port_state`,
/// in the entry of `name` in the space of state `state`: the entry takes the
/// right and its reference, or a user reference for a right it holds; the
/// port is unlocked.
///
/// # Panics
///
/// If `name` has no entry. If its entry holds the receive right, if its
/// user references would reach the maximum, or for a send-once right: not
/// yet. For a type that is not a right.
pub(super) fn copy_out(
    state: &mut SpaceState,
    name: PortName,
    message_type: MessageType,
    port: &Port,
    mut port_state: SpinlockGuard<'_, PortState>,
) {
    match message_type {
        MessageType::PortSend => {
            let SpaceState {
                map, reverse_map, ..
            } = state;
            // The name map's own pointer, for the reverse map, and its entry.
            let (Some(pointer), Some(entry)) = (map.lookup(name.key()), map.get_mut(name.key()))
            else {
                panic!("ipc_right_copyout: no entry");
            };
            let bits = &entry.bits;
            if bits.rights.contains(Rights::SEND) {
                assert!(
                    bits.user_references + 1 != u16::MAX,
                    "ipc_right_copyout: MACH_PORT_UREFS_MAX, not yet"
                );
                // It merges into the entry's right, which holds a reference.
                port_state.send_rights -= 1;
                port.release();
                drop(port_state);
            } else if bits.rights.contains(Rights::RECEIVE) {
                panic!("ipc_right_copyout: a receive right's entry, not yet")
            } else {
                drop(port_state);
                // As in GNU Mach, a failure leaves the entry out of the reverse map.
                let _ = reverse_map.insert(key(port), pointer);
                entry.name = name;
            }
            entry.bits.rights |= Rights::SEND;
            entry.bits.user_references += 1;
        }
        MessageType::PortSendOnce => {
            panic!("ipc_right_copyout: MACH_MSG_TYPE_PORT_SEND_ONCE, not yet")
        }
        _ => panic!("ipc_right_copyout: strange rights"),
    }
}
