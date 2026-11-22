// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use core::ptr::{self, NonNull};

use crate::arch::PhysAddr;
use crate::ipc::entry::{Entry, Rights};
use crate::ipc::object::Object;
use crate::ipc::port::port::{Port, Receiver};
use crate::ipc::port::port_name::{PortName, PortType};
use crate::ipc::port::state::PortState;
use crate::ipc::space::{Space, SpaceState};
use crate::reference::Reference;
use crate::sync::{SleepLockWriteGuard, SpinlockGuard};

impl Port {
    /// Its key in a reverse map: its physical address over 8, as ports are in
    /// the direct map.
    pub(super) fn reverse_key(&self) -> u64 {
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
    pub(in crate::ipc) fn lookup_write(
        &self,
        name: PortName,
    ) -> SleepLockWriteGuard<'_, SpaceState> {
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
    pub(in crate::ipc) fn reverse_lookup<'a>(
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

    /// Checks whether the port of `entry`'s send or send-once right, in the
    /// write-locked space, died, as `ipc_right_check` does:
    /// gives it locked and alive, or none if it died, for `make_dead_name`.
    ///
    /// # Panics
    ///
    /// If the space is dead, or the entry has no port.
    pub(super) fn live_port<'a>(&self, entry: &'a Entry) -> Option<SpinlockGuard<'a, PortState>> {
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

    /// The type of `entry`'s rights, in the write-locked, alive space, and its
    /// user references, as `ipc_right_info` gives them.
    ///
    /// # Panics
    ///
    /// As `live_port`. If the entry has a dead-name or message-accepted
    /// request: not yet.
    pub(in crate::ipc) fn entry_info(&self, entry: &Entry) -> (PortType, u16) {
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
}
