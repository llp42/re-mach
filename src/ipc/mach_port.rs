// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! Port calls: what tasks ask of a space's names.

use super::port::Port;
use super::space::Space;
use super::{MessageType, object};
use crate::error::Error;

/// Inserts the right `message_type` names, for `port`, in `space` as
/// `name`; the space takes the right and its reference.
///
/// # Errors
///
/// As `object::copy_out_name`.
///
/// # Panics
///
/// If `name` is null or dead: not yet; as `object::copy_out_name`.
pub fn insert_right(
    space: &Space,
    name: u32,
    port: &Port,
    message_type: MessageType,
) -> Result<(), Error> {
    assert!(
        name != 0 && name != u32::MAX,
        "mach_port_insert_right: an invalid name, not yet"
    );
    object::copy_out_name(space, port, message_type, name)
}
