// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Rights: what a space's entries hold of ports.

use core::ptr::{self, NonNull};

use super::MessageType;
use super::entry::{Entry, EntryBits, Rights};
use super::object::Object;
use super::port::{Port, PortName, PortState, PortType, Receiver};
use super::port_set::{self, PortSet};
use super::space::{Space, SpaceState};
use crate::arch::PhysAddr;
use crate::reference::Reference;
use crate::slab::CacheBox;
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
            port_state.receiver != Receiver::Space(NonNull::from(space)),
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
/// right, or a user reference for a right it holds; the port is unlocked.
/// Gives the entry if it is to take the right's reference, none if it keeps
/// its own and the caller drops the right's.
///
/// # Panics
///
/// If `name` has no entry. If its entry holds the receive right, if its
/// user references would reach the maximum, or for a send-once right: not
/// yet. For a type that is not a right.
pub(super) fn copy_out<'a>(
    state: &'a mut SpaceState,
    name: PortName,
    message_type: MessageType,
    port: &Port,
    mut port_state: SpinlockGuard<'_, PortState>,
) -> Option<&'a mut Entry> {
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
            let takes = if bits.rights.contains(Rights::SEND) {
                assert!(
                    bits.user_references + 1 != u16::MAX,
                    "ipc_right_copyout: MACH_PORT_UREFS_MAX, not yet"
                );
                // It merges into the entry's right, which holds a reference.
                port_state.send_rights -= 1;
                drop(port_state);
                false
            } else if bits.rights.contains(Rights::RECEIVE) {
                panic!("ipc_right_copyout: a receive right's entry, not yet")
            } else {
                drop(port_state);
                // As in GNU Mach, a failure leaves the entry out of the reverse map.
                let _ = reverse_map.insert(key(port), pointer);
                entry.name = name;
                true
            };
            entry.bits.rights |= Rights::SEND;
            entry.bits.user_references += 1;
            takes.then_some(entry)
        }
        MessageType::PortSendOnce => {
            panic!("ipc_right_copyout: MACH_MSG_TYPE_PORT_SEND_ONCE, not yet")
        }
        _ => panic!("ipc_right_copyout: strange rights"),
    }
}

/// Copies a send right from the entry of `name`, in the write-locked,
/// alive space of state `state`, as `ipc_right_copyin` does for
/// `MACH_MSG_TYPE_COPY_SEND`: gives the port's new send right with its
/// reference; the entry is unchanged.
///
/// # Panics
///
/// If the space is dead, `name` has no entry, or its entry has no port. If
/// the entry is a dead name or its port died, or it holds no send right:
/// not yet.
pub(super) fn copy_in(state: &SpaceState, name: PortName) -> Reference<Port> {
    assert!(state.active, "ipc_right_copyin: a dead space");
    let entry = state.lookup(name).expect("ipc_right_copyin: no entry");
    let rights = entry.bits.rights;
    assert!(
        !rights.contains(Rights::DEAD_NAME),
        "ipc_right_copyin: copy_dead, not yet"
    );
    // Dead send-once rights pass here, for `check`.
    assert!(
        rights.intersects(Rights::SEND | Rights::SEND_ONCE),
        "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
    );
    let Some(Object::Port(port)) = &entry.object else {
        panic!("ipc_right_copyin: a send right with no port");
    };
    let mut port_state = check(state, entry);
    assert!(
        rights.contains(Rights::SEND),
        "ipc_right_copyin: a send-once right, KERN_INVALID_RIGHT, not yet"
    );
    port_state.send_rights += 1;
    let port = port.clone();
    drop(port_state);
    port
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
    let Some(Object::Port(port)) = &entry.object else {
        panic!("ipc_right_check: a send right with no port");
    };
    let port_state = port.state.lock();
    assert!(
        port_state.active,
        "ipc_right_check: the port died, its entry to a dead name, not yet"
    );
    port_state
}

/// Releases a user reference of `name`'s right in the write-locked, alive
/// space of state `state`, as `ipc_right_dealloc` does; the space is
/// unlocked.
///
/// # Panics
///
/// If the space is dead, `name` has no entry, a dead name has no user
/// references, a request, an object or a message-accepted request, or a
/// send right has no user references, no port or a port with no send
/// right. If a dead name or a send right has more than one user reference,
/// a send right has a dead-name or message-accepted request, or for a
/// send-once or any other right: not yet. As `check` and
/// `SpaceState::deallocate`.
pub(super) fn deallocate(mut state: SleepLockWriteGuard<'_, SpaceState>, name: PortName) {
    assert!(state.active, "ipc_right_dealloc: a dead space");
    let entry = state
        .map
        .get(name.key())
        .expect("ipc_right_dealloc: no entry");
    let rights = entry.bits.rights.bits();
    if rights == Rights::DEAD_NAME.bits() {
        assert!(
            entry.bits.user_references > 0
                && entry.request == 0
                && entry.object.is_none()
                && !entry.bits.message_accepted,
            "ipc_right_dealloc: a dead name with no user references, a request, an object or a message-accepted request"
        );
        assert!(
            entry.bits.user_references == 1,
            "ipc_right_dealloc: a dead name's user references above 1, not yet"
        );
        state.deallocate(name);
        drop(state);
    } else if rights == Rights::SEND_ONCE.bits() {
        panic!("ipc_right_dealloc: MACH_PORT_TYPE_SEND_ONCE, not yet");
    } else if rights == Rights::SEND.bits() {
        assert!(
            entry.bits.user_references > 0,
            "ipc_right_dealloc: a send right with no user references"
        );
        let mut port_state = check(&state, entry);
        assert!(
            entry.bits.user_references == 1,
            "ipc_right_dealloc: a send right's user references above 1, not yet"
        );
        port_state.send_rights = port_state
            .send_rights
            .checked_sub(1)
            .expect("ipc_right_dealloc: a port with no send right");
        // No call makes a no-senders request yet: the last sender leaves none.
        assert!(
            entry.request == 0,
            "ipc_right_dealloc: ipc_right_dncancel, not yet"
        );
        assert!(
            !entry.bits.message_accepted,
            "ipc_right_dealloc: ipc_marequest_cancel, not yet"
        );
        drop(port_state);
        let SpaceState {
            map, reverse_map, ..
        } = &mut *state;
        let entry = map
            .get_mut(name.key())
            .expect("ipc_right_dealloc: no entry");
        // The entry's reference, ours now.
        let Some(Object::Port(port)) = entry.object.take() else {
            panic!("ipc_right_dealloc: a send right with no port");
        };
        reverse_map.remove(key(&port));
        state.deallocate(name);
        drop(state);
        // As ip_release, once nothing borrows the port.
        drop(port);
    } else if rights == (Rights::SEND | Rights::RECEIVE).bits() {
        panic!("ipc_right_dealloc: MACH_PORT_TYPE_SEND_RECEIVE, not yet");
    } else {
        panic!("ipc_right_dealloc: KERN_INVALID_RIGHT, not yet");
    }
}

/// Destroys the rights of `name`'s entry in `space`, write-locked as
/// `state` and alive, as `ipc_right_destroy` does; the space is unlocked.
///
/// # Panics
///
/// If the space is dead, `name` has no entry, a dead name has a request,
/// an object or a message-accepted request, a port set right has a request
/// or no set or a dead one, a receive right has no port or a dead one or
/// one another space receives on, or for rights that are none of Mach's. If
/// a receive right has a message-accepted or dead-name request, or for a
/// send or send-once right: not yet. As `SpaceState::deallocate`,
/// `PortSet::destroy`, `PortState::clear_receiver` and `Port::destroy`.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the set's and the port's guards are moved into their destroy"
)]
pub(super) fn destroy(
    space: &Space,
    mut state: SleepLockWriteGuard<'_, SpaceState>,
    name: PortName,
) {
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
        assert!(
            entry.request == 0,
            "ipc_right_destroy: a port set with a request"
        );
        let entry = state
            .map
            .get_mut(name.key())
            .expect("ipc_right_destroy: no entry");
        // The entry's reference, ours now.
        let Some(Object::PortSet(pointer)) = entry.object.take() else {
            panic!("ipc_right_destroy: a port set right with no set");
        };
        state.deallocate(name);
        // SAFETY: the reference we hold keeps the set alive, and this borrow
        // ends at `PortSet::destroy`'s return, before any free.
        let set = unsafe { pointer.as_ref() };
        let set_state = set.state.lock();
        assert!(set_state.active, "ipc_right_destroy: a dead port set");
        drop(state);
        if PortSet::destroy(set_state) {
            // As ips_check_unlock's io_free, with no lock held and nothing
            // borrowing the set.
            // SAFETY: every set is a `port_set::CACHE` box, given up once with
            // `into_raw` to the entry that held it, whose pointer we took; its
            // last reference is released, so nothing uses it after.
            drop(unsafe { CacheBox::from_raw(&port_set::CACHE, pointer) });
        }
    } else if rights == Rights::RECEIVE.bits() {
        assert!(
            !entry.bits.message_accepted,
            "ipc_right_destroy: ipc_marequest_cancel, not yet"
        );
        // Not a send right, so not in the reverse map.
        let entry = state
            .map
            .get_mut(name.key())
            .expect("ipc_right_destroy: no entry");
        // The entry's reference, ours now: under the space's lock, no one
        // sees the object clear before the port locks.
        let Some(Object::Port(port)) = entry.object.take() else {
            panic!("ipc_right_destroy: a receive right with no port");
        };
        let mut port_state = port.state.lock();
        assert!(
            port_state.active,
            "ipc_right_destroy: a dead port's receive right"
        );
        // As ipc_right_dncancel_macro: no request, none to cancel.
        assert!(
            entry.request == 0,
            "ipc_right_destroy: ipc_right_dncancel, not yet"
        );
        state.deallocate(name);
        drop(state);
        assert!(
            port_state.receiver == Receiver::Space(NonNull::from(space)),
            "ipc_right_destroy: another space's receive right"
        );
        port_state.clear_receiver();
        port.destroy(port_state);
        // As ipc_port_destroy's ipc_port_release, once nothing borrows the
        // port: the entry's reference, which frees it at zero.
        drop(port);
    } else if [
        Rights::SEND,
        Rights::SEND | Rights::RECEIVE,
        Rights::SEND_ONCE,
    ]
    .iter()
    .any(|kind| rights == kind.bits())
    {
        panic!("ipc_right_destroy: a send or send-once right, not yet");
    } else {
        panic!("ipc_right_destroy: strange type");
    }
}

/// The type of `entry`'s rights, in the write-locked, alive space of state
/// `state`, and its user references, as `ipc_right_info` gives them.
///
/// # Panics
///
/// As `check`. If the entry has a dead-name or message-accepted request:
/// not yet.
pub(super) fn info(state: &SpaceState, entry: &Entry) -> (PortType, u16) {
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
    (port_type, entry.bits.user_references)
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
        .get_mut(old_name.key())
        .expect("ipc_right_rename: no old entry");
    let EntryBits {
        rights,
        user_references,
        message_accepted,
    } = old.bits;
    let (request, object) = (old.request, old.object.take());
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
        let Some(Object::Port(port)) = &new.object else {
            panic!("ipc_right_rename: a send right with no port");
        };
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
    state.deallocate(old_name);
}
