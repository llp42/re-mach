// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.

use super::init::allocate;
use super::object::Object;
use super::offset::Offset;
use super::state::{Flags, ObjectState};
use crate::reference::Reference;

/// Another reference to `object` for a copy that shares it, as
/// `vm_object_copy_temporary` makes one for a temporary object: the source
/// and the copy each shadow it before writing; none if it isn't temporary.
pub fn copy_temporary(object: &Reference<Object>) -> Option<Reference<Object>> {
    let mut state = object.lock();
    if !state.flags.contains(Flags::TEMPORARY) {
        drop(state);
        return None;
    }
    // Symmetric: GNU Mach's use_shared_copy, for a delayed copy, is set only
    // by vm_map_fork.
    let copy = Reference::clone(object);
    state.flags.insert(Flags::SHADOWED);
    drop(state);
    Some(copy)
}

/// A new internal, temporary object of `size` bytes, with one reference
/// and a name port, in front of `source` from `offset`, as
/// `vm_object_shadow` makes it: its pages show where it has none. It takes
/// `source`'s reference.
///
/// # Panics
///
/// As `allocate`.
pub fn shadow(source: Reference<Object>, offset: Offset, size: usize) -> Reference<Object> {
    let object = allocate(size);
    let mut state = object.lock();
    // The reference moves: `source`'s count stays.
    state.shadow = Some(source);
    state.shadow_offset = offset;
    drop(state);
    object
}

/// The object and offset two adjoining ranges, `previous_size` bytes of
/// `previous` from `previous_offset` and `next_size` bytes of `next` from
/// `next_offset`, none for no object, have once joined, with a reference
/// to that object for the range they make, as `vm_object_coalesce` gives
/// them; none if they can't join.
///
/// # Panics
///
/// If they are the same object and contiguous, or only `next` has one and it
/// would take the range before it: not yet. As `ObjectState::collapse`.
pub fn coalesce(
    previous: Option<&Reference<Object>>,
    next: Option<&Reference<Object>>,
    previous_offset: Offset,
    next_offset: Offset,
    previous_size: usize,
    next_size: usize,
) -> Option<(Option<Reference<Object>>, Offset)> {
    let object = match (previous, next) {
        // Neither exists, so the offsets don't matter.
        (None, None) => return Some((None, Offset::from(0))),
        (Some(previous), Some(next)) if Reference::as_ptr(previous) == Reference::as_ptr(next) => {
            assert!(
                previous_offset + previous_size != next_offset,
                "vm_object_coalesce: the same object, not yet"
            );
            return None;
        }
        // Two different objects don't merge.
        (Some(_), Some(_)) => return None,
        (Some(object), None) | (None, Some(object)) => object,
    };
    let mut state = object.lock();
    ObjectState::collapse(&mut state, object);
    // Refused if pages outside these ranges may be in use.
    if object.references() > 1
        || state.flags.contains(Flags::PAGER_CREATED)
        || state.shadow.is_some()
        || state.copy.is_some()
        || state.paging_in_progress != 0
    {
        return None;
    }
    assert!(
        previous.is_some(),
        "vm_object_coalesce: the next object, not yet"
    );
    // Pages left from an earlier deallocation go, then it grows over the
    // range.
    let end = usize::from(previous_offset + previous_size);
    state.remove_pages(end..end + next_size);
    state.size = state.size.max(end + next_size);
    drop(state);
    Some((Some(object.clone()), previous_offset))
}
