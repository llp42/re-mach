// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::NonNull;

use crate::error::Error;
use crate::ipc::entry::Rights;
use crate::ipc::notify;
use crate::ipc::object::Object;
use crate::ipc::port::port::Receiver;
use crate::ipc::port::port_name::PortName;
use crate::ipc::port_set::{self, PortSet};
use crate::ipc::space::{Space, SpaceState};
use crate::slab::CacheBox;
use crate::sync::SleepLockWriteGuard;

impl SpaceState {
    /// Releases a user reference of `name`'s right in the write-locked, alive
    /// space of state `state`, as `ipc_right_dealloc` does, a send right whose
    /// port died turned into a dead name first; the space is unlocked.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, a dead name has no user
    /// references, a request, an object or a message-accepted request, or a
    /// send right has no user references, no port or a port with no send
    /// right. If a dead name has more than one user reference, a send right's
    /// last has a dead-name or message-accepted request, or for a send-once or
    /// any other right: not yet. As `live_port`, `make_dead_name` and
    /// `SpaceState::deallocate`.
    #[expect(
        clippy::too_many_lines,
        reason = "ipc_right_dealloc's arms, one per kind of right"
    )]
    pub(in crate::ipc) fn release_user_reference(
        mut state: SleepLockWriteGuard<'_, Self>,
        space: &Space,
        name: PortName,
    ) -> Result<(), Error> {
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
            let Some(mut port_state) = state.live_port(entry) else {
                state.make_dead_name(name);
                // As GNU Mach's goto: the dead name's arm.
                return Self::release_user_reference(state, space, name);
            };
            if entry.bits.user_references > 1 {
                assert!(
                    port_state.send_rights > 0,
                    "ipc_right_dealloc: a port with no send right"
                );
                drop(port_state);
                // One user reference fewer; the right and the port stay.
                state
                    .map
                    .get_mut(name.key())
                    .expect("ipc_right_dealloc: no entry")
                    .bits
                    .user_references -= 1;
                drop(state);
                return Ok(());
            }
            port_state.send_rights = port_state
                .send_rights
                .checked_sub(1)
                .expect("ipc_right_dealloc: a port with no send right");
            let no_senders = port_state.take_no_senders();
            assert!(
                entry.request == 0,
                "ipc_right_dealloc: ipc_right_dncancel, not yet"
            );
            assert!(
                !entry.bits.message_accepted,
                "ipc_right_dealloc: ipc_marequest_cancel, not yet"
            );
            drop(port_state);
            let Self {
                map, reverse_map, ..
            } = &mut *state;
            let entry = map
                .get_mut(name.key())
                .expect("ipc_right_dealloc: no entry");
            // The entry's reference, ours now.
            let Some(Object::Port(port)) = entry.object.take() else {
                panic!("ipc_right_dealloc: a send right with no port");
            };
            reverse_map.remove(port.reverse_key());
            state.deallocate(name);
            drop(state);
            // As ip_release, once nothing borrows the port.
            drop(port);
            if let Some((request, mscount)) = no_senders {
                // As ipc_notify_no_senders.
                notify::no_senders(request, mscount);
            }
        } else if rights == Rights::RECEIVE.bits() {
            // As ipc_right_dealloc: a receive right has no user references
            // to release.
            drop(state);
            return Err(Error::InvalidRight);
        } else if rights == (Rights::SEND | Rights::RECEIVE).bits() {
            assert!(
                entry.bits.user_references > 0,
                "ipc_right_dealloc: a send and receive right with no user references"
            );
            let Some(Object::Port(port)) = &entry.object else {
                panic!("ipc_right_dealloc: a send and receive right with no port");
            };
            let port = port.clone();
            let mut port_state = port.state.lock();
            assert!(
                port_state.active,
                "ipc_right_dealloc: a dead port's send and receive right"
            );
            assert!(
                port_state.receiver == Receiver::Space(NonNull::from(space))
                    && port_state.receiver_name == name,
                "ipc_right_dealloc: another space's or name's receive right"
            );
            assert!(
                port_state.send_rights > 0,
                "ipc_right_dealloc: a port with no send right"
            );
            port_state.send_rights -= 1;
            let no_senders = port_state.take_no_senders();
            drop(port_state);
            if entry.bits.user_references == 1 {
                let Self {
                    map, reverse_map, ..
                } = &mut *state;
                let entry = map
                    .get_mut(name.key())
                    .expect("ipc_right_dealloc: no entry");
                // The send right goes; the receive right stays.
                entry.bits.rights.remove(Rights::SEND);
                entry.bits.user_references = 0;
                reverse_map.remove(port.reverse_key());
            } else {
                state
                    .map
                    .get_mut(name.key())
                    .expect("ipc_right_dealloc: no entry")
                    .bits
                    .user_references -= 1;
            }
            drop(state);
            if let Some((request, mscount)) = no_senders {
                // As ipc_notify_no_senders.
                notify::no_senders(request, mscount);
            }
        } else {
            panic!("ipc_right_dealloc: KERN_INVALID_RIGHT, not yet");
        }
        Ok(())
    }

    /// Destroys the receive right of `name`'s send and receive entry in the
    /// write-locked, alive space, as `ipc_right_delta` does for a receive
    /// right's `-1`: the send right turns into a dead name, and the port
    /// dies, or a port-destroyed request takes it.
    ///
    /// # Panics
    ///
    /// If the space or the port is dead, the entry isn't a live send and
    /// receive right with user references, its name isn't the port's, a
    /// message-accepted request waits, or a dead-name request's user
    /// references would reach the maximum. As `PortState::clear_receiver`,
    /// `PortState::clear_receiver_limbo` and `Port::destroy`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the port's guard is moved into its destroy"
    )]
    pub(in crate::ipc) fn destroy_send_receive(
        mut state: SleepLockWriteGuard<'_, Self>,
        space: &Space,
        name: PortName,
    ) {
        assert!(state.active, "ipc_right_delta: a dead space");
        let entry = state
            .map
            .get(name.key())
            .expect("ipc_right_delta: no entry");
        assert!(
            entry.bits.rights.bits() == (Rights::SEND | Rights::RECEIVE).bits(),
            "ipc_right_delta: not a send and receive right"
        );
        assert!(
            entry.bits.user_references > 0,
            "ipc_right_delta: a send and receive right with no user references"
        );
        assert!(
            !entry.bits.message_accepted,
            "ipc_right_delta: ipc_marequest_cancel, not yet"
        );
        let entry = state
            .map
            .get_mut(name.key())
            .expect("ipc_right_delta: no entry");
        // The entry's reference, ours now: under the space's lock, no one
        // sees the object clear before the port locks.
        let Some(Object::Port(port)) = entry.object.take() else {
            panic!("ipc_right_delta: a send and receive right with no port");
        };
        let dead_name = entry.dead_name.take();
        // The send right turns into a dead name; its dead-name request, if
        // one, keeps a user reference for the notification's name.
        entry.bits.rights = Rights::DEAD_NAME;
        if dead_name.is_some() {
            entry.bits.user_references = entry
                .bits
                .user_references
                .checked_add(1)
                .expect("ipc_right_delta: MACH_PORT_UREFS_MAX, not yet");
        }
        state.reverse_map.remove(port.reverse_key());
        let mut port_state = port.state.lock();
        assert!(port_state.active, "ipc_right_delta: a dead port");
        assert!(
            port_state.receiver == Receiver::Space(NonNull::from(space))
                && port_state.receiver_name == name,
            "ipc_right_delta: another space's or name's receive right"
        );
        if let Some(request) = port_state.take_port_destroyed() {
            // As ipc_notify_port_destroyed: the port lives on with
            // `port`'s reference, and a dead-name request goes with it.
            port_state.clear_receiver_limbo();
            drop(port_state);
            drop(state);
            drop(dead_name);
            notify::port_destroyed(request, port);
            return;
        }
        let send_once = port_state.no_senders.take();
        port_state.clear_receiver();
        port.destroy(port_state);
        drop(state);
        drop(port);
        if let Some(request) = send_once {
            notify::send_once(request);
        }
        if let Some(request) = dead_name {
            notify::dead_name(request, name);
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
    pub(in crate::ipc) fn destroy_rights(
        mut state: SleepLockWriteGuard<'_, Self>,
        space: &Space,
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
            let dead_name = entry.dead_name.take();
            let mut port_state = port.state.lock();
            assert!(
                port_state.active,
                "ipc_right_destroy: a dead port's receive right"
            );
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
            if let Some(request) = port_state.take_port_destroyed() {
                // As ipc_notify_port_destroyed: the port lives on with
                // `port`'s reference.
                port_state.clear_receiver_limbo();
                drop(port_state);
                notify::port_destroyed(request, port);
                if let Some(request) = dead_name {
                    // As ipc_notify_port_deleted.
                    notify::port_deleted(request, name);
                }
            } else {
                let send_once = port_state.no_senders.take();
                port_state.clear_receiver();
                port.destroy(port_state);
                // As ipc_port_destroy's ipc_port_release, once nothing
                // borrows the port: the entry's reference, which frees it at
                // zero.
                drop(port);
                if let Some(request) = send_once {
                    notify::send_once(request);
                }
                if let Some(request) = dead_name {
                    notify::port_deleted(request, name);
                }
            }
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
}
