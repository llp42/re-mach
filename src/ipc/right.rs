// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Rights: what a space's entries hold of ports.

use core::ptr::{self, NonNull};

use super::MessageType;
use super::entry::{Entry, EntryBits, Rights};
use super::object::Object;
use super::port::{Port, PortName, PortState, PortType};
use super::space::{Space, SpaceState};
use crate::arch::PhysAddr;
use crate::sync::{SleepLockWriteGuard, SpinlockGuard};

/// `port`'s key in a reverse map: its physical address over 8, as ports
/// are in the direct map.
fn key(port: &Port) -> u64 {
    u64::try_from(PhysAddr::from_direct_map(ptr::from_ref(port)).0 >> 3).unwrap()
}

/// Write-locks `space`, which must be alive and hold rights under `name`,
/// as `ipc_right_lookup_write` does: gives the guard, whose state has the
/// entry.
///
/// # Panics
///
/// If the space is dead, or `name` holds no rights: not yet.
pub(super) fn lookup_write(space: &Space, name: PortName) -> SleepLockWriteGuard<'_, SpaceState> {
    let state = space.state.write();
    assert!(
        state.active,
        "ipc_right_lookup_write: KERN_INVALID_TASK, not yet"
    );
    assert!(
        state.lookup(name).is_some(),
        "ipc_right_lookup_write: KERN_INVALID_NAME, not yet"
    );
    state
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

/// Checks whether the port of `entry`'s send or send-once right, in the
/// write-locked space of state `state`, died, as `ipc_right_check` does:
/// gives it locked and alive.
///
/// # Panics
///
/// If the space is dead, or the entry has no port. If the port died: not
/// yet.
fn check<'a>(state: &SpaceState, entry: &'a Entry) -> SpinlockGuard<'a, PortState> {
    assert!(state.active, "ipc_right_check: a dead space");
    let Some(Object::Port(port)) = entry.object else {
        panic!("ipc_right_check: a send right with no port");
    };
    // SAFETY: the entry holds a reference to its port, and the space's lock
    // keeps the entry.
    let port = unsafe { port.as_ref() };
    let port_state = port.state.lock();
    assert!(
        port_state.active,
        "ipc_right_check: the port died, its entry to a dead name, not yet"
    );
    port_state
}

/// Destroys the rights of `name`'s entry in the write-locked, alive space
/// of state `state`, as `ipc_right_destroy` does; the space is unlocked.
///
/// # Panics
///
/// If the space is dead, `name` has no entry, a dead name has a request,
/// an object or a message-accepted request, or for rights that are none of
/// Mach's. For a port set's or a port's rights: not yet. As
/// `SpaceState::deallocate`.
pub(super) fn destroy(mut state: SleepLockWriteGuard<'_, SpaceState>, name: PortName) {
    assert!(state.active, "ipc_right_destroy: a dead space");
    let entry = state
        .map
        .get(name.key())
        .expect("ipc_right_destroy: no entry");
    let rights = entry.bits.rights.bits();
    if rights == Rights::DEAD_NAME.bits() {
        assert!(
            entry.request == 0 && entry.object.is_none() && !entry.bits.message_accepted,
            "ipc_right_destroy: a dead name with a request, an object or a message-accepted request"
        );
        state.deallocate(name);
        drop(state);
    } else if rights == Rights::PORT_SET.bits() {
        panic!("ipc_right_destroy: MACH_PORT_TYPE_PORT_SET, not yet");
    } else if [
        Rights::SEND,
        Rights::RECEIVE,
        Rights::SEND | Rights::RECEIVE,
        Rights::SEND_ONCE,
    ]
    .iter()
    .any(|kind| rights == kind.bits())
    {
        panic!("ipc_right_destroy: a send, receive or send-once right, not yet");
    } else {
        panic!("ipc_right_destroy: strange type");
    }
}

/// The type of `entry`'s rights, in the write-locked, alive space of state
/// `state`, as `ipc_right_info` gives it.
///
/// # Panics
///
/// As `check`. If the entry has a dead-name or message-accepted request:
/// not yet.
pub(super) fn info(state: &SpaceState, entry: &Entry) -> PortType {
    if entry
        .bits
        .rights
        .intersects(Rights::SEND | Rights::SEND_ONCE)
    {
        // Alive, so it is unlocked again.
        drop(check(state, entry));
    }
    let port_type = PortType::from(entry.bits.rights);
    assert!(
        entry.request == 0,
        "ipc_right_info: MACH_PORT_TYPE_DNREQUEST, not yet"
    );
    assert!(
        !entry.bits.message_accepted,
        "ipc_right_info: MACH_PORT_TYPE_MAREQUEST, not yet"
    );
    port_type
}

/// Moves the rights of `old_name`'s entry to `new_name`'s, new and holding
/// none, in the write-locked, alive space of state `state`, as
/// `ipc_right_rename` does; the old entry is freed.
///
/// # Panics
///
/// If the space is dead, either name has no entry, or the new entry holds
/// rights, or for rights that are none of Mach's. If the old entry has a
/// dead-name or message-accepted request, or its rights are a receive,
/// send-once, port set or dead name right: not yet. As `SpaceState::deallocate`.
pub(super) fn rename(state: &mut SpaceState, old_name: PortName, new_name: PortName) {
    assert!(state.active, "ipc_right_rename: a dead space");
    let SpaceState {
        map, reverse_map, ..
    } = state;
    let old = map
        .get(old_name.key())
        .expect("ipc_right_rename: no old entry");
    let EntryBits {
        rights,
        user_references,
        message_accepted,
    } = old.bits;
    let (request, object) = (old.request, old.object);
    assert!(
        request == 0,
        "ipc_right_rename: ipc_right_check and ipc_port_dnrename, not yet"
    );
    assert!(
        !message_accepted,
        "ipc_right_rename: ipc_marequest_rename, not yet"
    );
    // The name map's own pointer, for the reverse map, and its entry.
    let (Some(pointer), Some(new)) = (map.lookup(new_name.key()), map.get_mut(new_name.key()))
    else {
        panic!("ipc_right_rename: no new entry");
    };
    assert!(
        !new.in_use() && new.bits.user_references == 0 && !new.bits.message_accepted,
        "ipc_right_rename: a new entry with rights"
    );
    new.bits = EntryBits {
        rights,
        user_references,
        message_accepted,
    };
    new.request = request;
    new.object = object;
    if rights.bits() == Rights::SEND.bits() {
        let Some(Object::Port(port)) = object else {
            panic!("ipc_right_rename: a send right with no port");
        };
        // SAFETY: the entry holds a reference to its port, and the space's
        // lock keeps the entry.
        let port = unsafe { port.as_ref() };
        reverse_map.remove(key(port));
        new.name = new_name;
        // As in GNU Mach, a failure leaves the entry out of the reverse map.
        let _ = reverse_map.insert(key(port), pointer);
    } else if rights.contains(Rights::RECEIVE) {
        panic!("ipc_right_rename: MACH_PORT_TYPE_RECEIVE, not yet");
    } else if rights.bits() == Rights::SEND_ONCE.bits() {
        panic!("ipc_right_rename: MACH_PORT_TYPE_SEND_ONCE, not yet");
    } else if rights.bits() == Rights::PORT_SET.bits() {
        panic!("ipc_right_rename: MACH_PORT_TYPE_PORT_SET, not yet");
    } else if rights.bits() == Rights::DEAD_NAME.bits() {
        panic!("ipc_right_rename: MACH_PORT_TYPE_DEAD_NAME, not yet");
    } else {
        panic!("ipc_right_rename: strange rights");
    }
    map.get_mut(old_name.key())
        .expect("ipc_right_rename: no old entry")
        .object = None;
    state.deallocate(old_name);
}
