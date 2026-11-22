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

impl Port {
    /// Its key in a reverse map: its physical address over 8, as ports are in
    /// the direct map.
    fn reverse_key(&self) -> u64 {
        u64::try_from(PhysAddr::from_direct_map(ptr::from_ref(self)).0 >> 3).unwrap()
    }
}

impl Space {
    /// Write-locks it, which must be alive and hold rights under `name`, as
    /// `ipc_right_lookup_write` does: gives the guard, whose state has the
    /// entry.
    ///
    /// # Panics
    ///
    /// If the space is dead, or `name` holds no rights: not yet.
    pub(super) fn lookup_write(&self, name: PortName) -> SleepLockWriteGuard<'_, SpaceState> {
        let state = self.state.write();
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
}

impl SpaceState {
    /// Looks for a send or receive right for `port` in `space`, whose state it
    /// is, as `ipc_right_reverse` does: gives its entry's name, with the port
    /// locked.
    ///
    /// # Panics
    ///
    /// If the space receives on `port` under a null name, or under a name
    /// whose entry is not the port's receive right.
    pub(super) fn reverse_lookup<'a>(
        &self,
        space: &Space,
        port: &'a Port,
    ) -> Option<(PortName, SpinlockGuard<'a, PortState>)> {
        let port_state = port.state.lock();
        // A dead port has none.
        if port_state.active {
            if port_state.receiver == Receiver::Space(NonNull::from(space)) {
                let name = port_state.receiver_name;
                assert!(
                    name != PortName::NULL,
                    "ipc_right_reverse: a receive right with a null name"
                );
                let entry = self
                    .lookup(name)
                    .expect("ipc_right_reverse: no receive right's entry");
                let Some(Object::Port(object)) = &entry.object else {
                    panic!("ipc_right_reverse: a receive right's entry with no port");
                };
                assert!(
                    entry.bits.rights.contains(Rights::RECEIVE)
                        && Reference::as_ptr(object) == NonNull::from(port),
                    "ipc_right_reverse: an entry that is not the port's receive right"
                );
                return Some((name, port_state));
            }
            if let Some(entry) = self.reverse_map.lookup(port.reverse_key()) {
                // SAFETY: the reverse map holds copies of the name map's pointers,
                // to entries it still holds, and `self` keeps the name map shared,
                // so no exclusive lend is alive.
                let name = unsafe { entry.as_ref() }.name;
                return Some((name, port_state));
            }
        }
        drop(port_state);
        None
    }

    /// Puts the right `message_type` names, for `port`, of state `port_state`,
    /// in the entry of `name`: the entry takes the right, or a user reference
    /// for a right it holds; the port is unlocked.
    /// Gives the entry if it is to take the right's reference, none if it keeps
    /// its own and the caller drops the right's.
    ///
    /// # Panics
    ///
    /// If `name` has no entry, its entry holds the receive right with other
    /// rights or user references, or a send-once right's entry is in use or
    /// the port has none. If its user references would reach the maximum: not
    /// yet. For a type that is not a right.
    pub(super) fn put_right(
        &mut self,
        name: PortName,
        message_type: MessageType,
        port: &Port,
        mut port_state: SpinlockGuard<'_, PortState>,
    ) -> Option<&mut Entry> {
        match message_type {
            MessageType::PortSend => {
                let Self {
                    map, reverse_map, ..
                } = self;
                // The name map's own pointer, for the reverse map, and its entry.
                let (Some(pointer), Some(entry)) =
                    (map.lookup(name.key()), map.get_mut(name.key()))
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
                    assert!(
                        bits.rights.bits() == Rights::RECEIVE.bits() && bits.user_references == 0,
                        "ipc_right_copyout: a receive right's entry with other rights or user references"
                    );
                    // The send right moves to the entry; the entry keeps its own reference.
                    drop(port_state);
                    false
                } else {
                    drop(port_state);
                    // As in GNU Mach, a failure leaves the entry out of the reverse map.
                    let _ = reverse_map.insert(port.reverse_key(), pointer);
                    entry.name = name;
                    true
                };
                entry.bits.rights |= Rights::SEND;
                entry.bits.user_references += 1;
                takes.then_some(entry)
            }
            MessageType::PortSendOnce => {
                let entry = self
                    .map
                    .get_mut(name.key())
                    .expect("ipc_right_copyout: no entry");
                assert!(
                    !entry.in_use() && port_state.send_once_rights > 0,
                    "ipc_right_copyout: a send-once right to an entry in use, or none"
                );
                drop(port_state);
                // The entry takes the right and its reference.
                entry.bits.rights |= Rights::SEND_ONCE;
                entry.bits.user_references = 1;
                Some(entry)
            }
            _ => panic!("ipc_right_copyout: strange rights"),
        }
    }

    /// Copies a send right from the entry of `name`, in the write-locked,
    /// alive space, as `ipc_right_copyin` does for
    /// `MACH_MSG_TYPE_COPY_SEND`: gives the port's new send right with its
    /// reference, the entry unchanged; or none if the port died, the entry
    /// turned into a dead name (`copy_dead`).
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, or its entry has no port. If
    /// the entry is a dead name, or holds no send right: not yet. As
    /// `make_dead_name`.
    pub(super) fn copy_send(&mut self, name: PortName) -> Option<Reference<Port>> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        let rights = entry.bits.rights;
        assert!(
            !rights.contains(Rights::DEAD_NAME),
            "ipc_right_copyin: copy_dead, not yet"
        );
        // Dead send-once rights pass here, for `live_port`.
        assert!(
            rights.intersects(Rights::SEND | Rights::SEND_ONCE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        let Some(Object::Port(port)) = &entry.object else {
            panic!("ipc_right_copyin: a send right with no port");
        };
        let Some(mut port_state) = self.live_port(entry) else {
            self.make_dead_name(name);
            // copy_dead.
            return None;
        };
        assert!(
            rights.contains(Rights::SEND),
            "ipc_right_copyin: a send-once right, KERN_INVALID_RIGHT, not yet"
        );
        port_state.send_rights += 1;
        let port = port.clone();
        drop(port_state);
        Some(port)
    }

    /// Moves a send right out of `name`'s entry, in the write-locked, alive
    /// space, as `ipc_right_copyin` does for
    /// `MACH_MSG_TYPE_MOVE_SEND`: gives the right with its reference. The
    /// entry's last user reference gives the entry's own, leaving the entry no
    /// rights, out of the reverse map, for the caller to free; another makes
    /// the port a new send right and reference.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, the entry has no user
    /// references or no port, or its port no send right. If the entry is a dead
    /// name or holds no send right, its port died, or its last user reference
    /// is also a receive right's or has a dead-name or message-accepted
    /// request: not yet.
    pub(super) fn move_send(&mut self, name: PortName) -> Reference<Port> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        let rights = entry.bits.rights;
        assert!(
            !rights.contains(Rights::DEAD_NAME),
            "ipc_right_copyin: move_dead, not yet"
        );
        // Dead send-once rights pass here, for `live_port`.
        assert!(
            rights.intersects(Rights::SEND | Rights::SEND_ONCE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        assert!(
            entry.bits.user_references > 0,
            "ipc_right_copyin: no user references"
        );
        let Some(mut port_state) = self.live_port(entry) else {
            panic!("ipc_right_check: the port died, its entry to a dead name, move_dead, not yet");
        };
        assert!(
            rights.contains(Rights::SEND),
            "ipc_right_copyin: a send-once right, KERN_INVALID_RIGHT, not yet"
        );
        assert!(
            port_state.send_rights > 0,
            "ipc_right_copyin: a port with no send right"
        );
        if entry.bits.user_references == 1 {
            assert!(
                !rights.contains(Rights::RECEIVE),
                "ipc_right_copyin: a send and receive right's last user reference, not yet"
            );
            // ipc_right_dncancel_macro.
            assert!(entry.request == 0, "ipc_right_dncancel, not yet");
            assert!(
                !entry.bits.message_accepted,
                "ipc_right_copyin: ipc_marequest_cancel, not yet"
            );
            drop(port_state);
            let Self {
                map, reverse_map, ..
            } = self;
            let entry = map.get_mut(name.key()).expect("ipc_right_copyin: no entry");
            let Some(Object::Port(port)) = entry.object.take() else {
                panic!("ipc_right_copyin: a send right with no port");
            };
            reverse_map.remove(port.reverse_key());
            entry.bits.rights.remove(Rights::SEND);
            entry.bits.user_references = 0;
            port
        } else {
            port_state.send_rights += 1;
            let Some(Object::Port(port)) = &entry.object else {
                panic!("ipc_right_copyin: a send right with no port");
            };
            let port = port.clone();
            drop(port_state);
            self.map
                .get_mut(name.key())
                .expect("ipc_right_copyin: no entry")
                .bits
                .user_references -= 1;
            port
        }
    }

    /// Makes a send right from the receive right of `name`'s entry, in the
    /// write-locked, alive `space`, whose state it is, as `ipc_right_copyin`
    /// does for `MACH_MSG_TYPE_MAKE_SEND`: gives the port's new send right with
    /// its reference; the entry is unchanged.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, or its receive right has no
    /// port, a dead one or one received under another name or space. If the
    /// entry holds no receive right: not yet.
    pub(super) fn make_send(&self, space: &Space, name: PortName) -> Reference<Port> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        assert!(
            entry.bits.rights.contains(Rights::RECEIVE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        let Some(Object::Port(port)) = &entry.object else {
            panic!("ipc_right_copyin: a receive right with no port");
        };
        let mut port_state = port.state.lock();
        assert!(
            port_state.active,
            "ipc_right_copyin: a dead port's receive right"
        );
        assert!(
            port_state.receiver_name == name
                && port_state.receiver == Receiver::Space(NonNull::from(space)),
            "ipc_right_copyin: a receive right under another name or space"
        );
        port_state.make_send_count += 1;
        port_state.send_rights += 1;
        let port = port.clone();
        drop(port_state);
        port
    }

    /// Makes a send-once right from the receive right of `name`'s entry, in the
    /// write-locked, alive `space`, whose state it is, as `ipc_right_copyin`
    /// does for `MACH_MSG_TYPE_MAKE_SEND_ONCE`: gives the port's new send-once
    /// right with its reference; the entry is unchanged.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, or its receive right has no
    /// port, a dead one or one received under another name or space. If the
    /// entry holds no receive right: not yet.
    pub(super) fn make_send_once(&self, space: &Space, name: PortName) -> Reference<Port> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        assert!(
            entry.bits.rights.contains(Rights::RECEIVE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        let Some(Object::Port(port)) = &entry.object else {
            panic!("ipc_right_copyin: a receive right with no port");
        };
        let mut port_state = port.state.lock();
        assert!(
            port_state.active,
            "ipc_right_copyin: a dead port's receive right"
        );
        assert!(
            port_state.receiver_name == name
                && port_state.receiver == Receiver::Space(NonNull::from(space)),
            "ipc_right_copyin: a receive right under another name or space"
        );
        port_state.send_once_rights += 1;
        let port = port.clone();
        drop(port_state);
        port
    }

    /// Moves the send-once right of `name`'s entry out of the write-locked,
    /// alive space, as `ipc_right_copyin` does for
    /// `MACH_MSG_TYPE_MOVE_SEND_ONCE`: gives the right with the entry's
    /// reference; the entry is left with no rights, for the caller to free.
    ///
    /// # Panics
    ///
    /// If the space is dead, `name` has no entry, or the entry has no user
    /// references or no port. If the entry is a dead name, holds no send-once
    /// right, its port died, or it has a dead-name request: not yet. If a
    /// send-once right's entry holds other rights, other user references or a
    /// message-accepted request, or its port has no send-once right.
    pub(super) fn move_send_once(&mut self, name: PortName) -> Reference<Port> {
        assert!(self.active, "ipc_right_copyin: a dead space");
        let entry = self.lookup(name).expect("ipc_right_copyin: no entry");
        let rights = entry.bits.rights;
        assert!(
            !rights.contains(Rights::DEAD_NAME),
            "ipc_right_copyin: move_dead, not yet"
        );
        // Dead send rights pass here, for `live_port`.
        assert!(
            rights.intersects(Rights::SEND | Rights::SEND_ONCE),
            "ipc_right_copyin: KERN_INVALID_RIGHT, not yet"
        );
        assert!(
            entry.bits.user_references > 0,
            "ipc_right_copyin: no user references"
        );
        let Some(port_state) = self.live_port(entry) else {
            panic!("ipc_right_check: the port died, its entry to a dead name, move_dead, not yet");
        };
        assert!(
            rights.contains(Rights::SEND_ONCE),
            "ipc_right_copyin: a send right, KERN_INVALID_RIGHT, not yet"
        );
        assert!(
            rights.bits() == Rights::SEND_ONCE.bits()
                && entry.bits.user_references == 1
                && !entry.bits.message_accepted,
            "ipc_right_copyin: a send-once right's rights, user references or message-accepted request"
        );
        assert!(
            port_state.send_once_rights > 0,
            "ipc_right_copyin: a port with no send-once right"
        );
        // ipc_right_dncancel_macro.
        assert!(entry.request == 0, "ipc_right_dncancel, not yet");
        drop(port_state);
        let entry = self
            .map
            .get_mut(name.key())
            .expect("ipc_right_copyin: no entry");
        let Some(Object::Port(port)) = entry.object.take() else {
            panic!("ipc_right_copyin: a send-once right with no port");
        };
        entry.bits.rights.remove(Rights::SEND_ONCE);
        port
    }

    /// Whether a copy-in of `message_type` from `entry`, in the locked, alive
    /// space, would succeed, as `ipc_right_copyin_check`
    /// tells; so far for `MACH_MSG_TYPE_MAKE_SEND_ONCE`: the entry holds a
    /// receive right.
    ///
    /// # Panics
    ///
    /// If the space is dead. For another type: not yet.
    pub(super) fn can_copy_in(&self, entry: &Entry, message_type: MessageType) -> bool {
        assert!(self.active, "ipc_right_copyin_check: a dead space");
        match message_type {
            MessageType::MakeSendOnce => entry.bits.rights.contains(Rights::RECEIVE),
            _ => panic!(
                "ipc_right_copyin_check: type {}, not yet",
                message_type.number()
            ),
        }
    }

    /// Checks whether the port of `entry`'s send or send-once right, in the
    /// write-locked space, died, as `ipc_right_check` does:
    /// gives it locked and alive, or none if it died, for `make_dead_name`.
    ///
    /// # Panics
    ///
    /// If the space is dead, or the entry has no port.
    fn live_port<'a>(&self, entry: &'a Entry) -> Option<SpinlockGuard<'a, PortState>> {
        assert!(self.active, "ipc_right_check: a dead space");
        let Some(Object::Port(port)) = &entry.object else {
            panic!("ipc_right_check: a send right with no port");
        };
        let port_state = port.state.lock();
        if port_state.active {
            return Some(port_state);
        }
        drop(port_state);
        None
    }

    /// Turns `name`'s send right, whose port died, into a dead name in the
    /// write-locked, alive space, as `ipc_right_check` does
    /// once it finds the port dead: the entry keeps its user references and
    /// releases the port's reference, which the reverse map no longer holds.
    ///
    /// # Panics
    ///
    /// If `name` has no entry, no user references, or rights other than a send
    /// or send-once right, or the entry has no port. For a send-once right, or
    /// a message-accepted or dead-name request: not yet.
    fn make_dead_name(&mut self, name: PortName) {
        let Self {
            map, reverse_map, ..
        } = self;
        let entry = map.get_mut(name.key()).expect("ipc_right_check: no entry");
        let rights = entry.bits.rights.bits();
        assert!(
            entry.bits.user_references > 0,
            "ipc_right_check: no user references"
        );
        assert!(
            rights != Rights::SEND_ONCE.bits(),
            "ipc_right_check: a send-once right, not yet"
        );
        assert!(
            rights == Rights::SEND.bits(),
            "ipc_right_check: neither a send nor a send-once right"
        );
        assert!(
            !entry.bits.message_accepted,
            "ipc_right_check: ipc_marequest_cancel, not yet"
        );
        let Some(Object::Port(port)) = entry.object.take() else {
            panic!("ipc_right_check: a send right with no port");
        };
        reverse_map.remove(port.reverse_key());
        // As ipc_port_release: the entry's reference, the last frees the port.
        drop(port);
        entry.bits.rights = Rights::DEAD_NAME;
        // No call makes a dead-name request yet: none adds a user reference.
        assert!(
            entry.request == 0,
            "ipc_right_check: a dead-name request, not yet"
        );
    }

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
    pub(super) fn release_user_reference(mut state: SleepLockWriteGuard<'_, Self>, name: PortName) {
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
                Self::release_user_reference(state, name);
                return;
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
                return;
            }
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
    pub(super) fn destroy_rights(
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

    /// The type of `entry`'s rights, in the write-locked, alive space, and its
    /// user references, as `ipc_right_info` gives them.
    ///
    /// # Panics
    ///
    /// As `live_port`. If the entry has a dead-name or message-accepted
    /// request: not yet.
    pub(super) fn entry_info(&self, entry: &Entry) -> (PortType, u16) {
        if entry
            .bits
            .rights
            .intersects(Rights::SEND | Rights::SEND_ONCE)
        {
            // Alive, so it is unlocked again.
            drop(self.live_port(entry));
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
    /// none, in the write-locked, alive space, as `ipc_right_rename` does; the
    /// old entry is freed.
    ///
    /// # Panics
    ///
    /// If the space is dead, either name has no entry, or the new entry holds
    /// rights, or for rights that are none of Mach's. If the old entry has a
    /// dead-name or message-accepted request, or its rights are a receive,
    /// send-once, port set or dead name right: not yet. As
    /// `SpaceState::deallocate`.
    pub(super) fn move_rights(&mut self, old_name: PortName, new_name: PortName) {
        assert!(self.active, "ipc_right_rename: a dead space");
        let Self {
            map, reverse_map, ..
        } = self;
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
            reverse_map.remove(port.reverse_key());
            new.name = new_name;
            // As in GNU Mach, a failure leaves the entry out of the reverse map.
            let _ = reverse_map.insert(port.reverse_key(), pointer);
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
        self.deallocate(old_name);
    }
}
