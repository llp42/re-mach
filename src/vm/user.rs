// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! The virtual memory calls of Mach's interface.

use super::map::change::{Change, Placement, RangeCheck, Source};
use super::map::copy::MapCopy;
use super::map::map::Map;
use super::object::{self, object::Object, offset::Offset};
use super::page;
use super::proxy::Proxy;
use super::{Inheritance, Protection, VirtualAddress, Wire};
use crate::arch::PAGE_SIZE;
use crate::error::Error;
use crate::ipc::port::kernel_object::KernelObject;
use crate::ipc::port::port::Port;
use crate::ipc::port::send_right::SendRight;
use crate::reference::Reference;
use crate::slab::CacheBox;
use crate::sync::Spinlock;

/// The most an unprivileged host may wire, as GNU Mach caps it.
const WIRE_LIMIT: usize = 8 << 20;

/// What `map` maps of a memory object.
pub enum Mapping {
    /// The object itself.
    Object,
    /// A copy of it.
    Copy,
}

/// Maps `size` bytes of zero-filled memory into `map`, as `placement`
/// says, a fixed address rounded down to a page, `size` rounded up to
/// pages; returns where they start, or 0 for no bytes. They may be read
/// and written, at most anything, and child tasks get a copy.
///
/// # Errors
///
/// `InvalidArgument` if `size` rounds up past the last address, and as
/// `Map::enter`.
///
/// # Panics
///
/// As `Map::enter`.
pub fn allocate(map: &Map, placement: Placement, size: usize) -> Result<VirtualAddress, Error> {
    if size == 0 {
        return Ok(VirtualAddress::from(0));
    }
    // No mask; VM_PROT_DEFAULT, VM_PROT_ALL and VM_INHERIT_DEFAULT.
    enter(
        map,
        placement,
        size,
        0,
        None,
        Offset::from(0),
        Protection::READ | Protection::WRITE,
        Protection::all(),
        Inheritance::Copy,
    )
}

/// Maps `size` bytes of `memory_object` from `offset`, as `mapping` says,
/// or of zero-filled memory for none, into `map`, as `placement` says, at
/// an address with no bit of `mask` set, a fixed one rounded down to a
/// page, `size` rounded up to pages; returns where they start. They allow
/// `protection`, at most `max_protection`, and child tasks get
/// `inheritance` of them.
///
/// # Errors
///
/// `InvalidArgument` if a protection holds more than reading, writing and
/// running, or `size` is 0 or rounds up past the last address, `NoSpace`
/// if the start has a bit of `mask` set, `ProtectionFailure` for a copy a
/// proxy doesn't allow reading, `InvalidArgument` for a range past a
/// proxy's, and as `Map::enter`.
///
/// # Panics
///
/// If `memory_object` stands for no memory object or proxy, or for
/// `Mapping::Copy` of one: not yet. As `object::enter` and `enter`.
#[expect(clippy::too_many_arguments, reason = "vm_map's parameters")]
pub fn map(
    map: &Map,
    placement: Placement,
    size: usize,
    mask: usize,
    memory_object: Option<&Reference<Port>>,
    offset: Offset,
    mapping: Mapping,
    protection: Protection,
    max_protection: Protection,
    inheritance: Inheritance,
) -> Result<VirtualAddress, Error> {
    // `inheritance` needs no check: its type holds only valid values.
    if !Protection::all().contains(protection) || !Protection::all().contains(max_protection) {
        return Err(Error::InvalidArgument);
    }
    if size == 0 {
        return Err(Error::InvalidArgument);
    }
    // Past the top, GNU Mach's rounding wraps to 0, which Map::enter refuses.
    let size = size.checked_next_multiple_of(PAGE_SIZE).unwrap_or(0);
    // IP_NULL: no object, so no offset or copy.
    let (object, offset, mapping, protection, max_protection) = if let Some(port) = memory_object {
        if let Some(object) = object::external::enter(port, size) {
            (Some(object), offset, mapping, protection, max_protection)
        } else {
            let Some(proxy) = port.to_proxy() else {
                panic!("vm_map: memory_object_proxy_lookup, not yet");
            };
            // As memory_object_proxy_lookup and vm_map's use of it: the
            // proxy's protection and range, for its object.
            let allowed = proxy.max_protection();
            let range = proxy.range();
            let (protection, max_protection) = if matches!(mapping, Mapping::Copy) {
                if !allowed.contains(Protection::READ) {
                    return Err(Error::ProtectionFailure);
                }
                (protection, max_protection)
            } else {
                (protection & allowed, max_protection & allowed)
            };
            let end = range
                .start
                .checked_add(usize::from(offset))
                .and_then(|value| value.checked_add(size));
            if end.is_none_or(|end| end > range.end) {
                return Err(Error::InvalidArgument);
            }
            let Some(object) = object::external::enter(proxy.object(), size) else {
                return Err(Error::InvalidArgument);
            };
            (
                Some(object),
                offset + range.start,
                mapping,
                protection,
                max_protection,
            )
        }
    } else {
        (
            None,
            Offset::from(0),
            Mapping::Object,
            protection,
            max_protection,
        )
    };
    assert!(
        matches!(mapping, Mapping::Object),
        "vm_map: vm_object_copy_strategically, not yet"
    );
    // A failure drops the object's reference, as vm_object_deallocate.
    enter(
        map,
        placement,
        size,
        mask,
        object,
        offset,
        protection,
        max_protection,
        inheritance,
    )
}

/// Makes a proxy for the region `address` is in of `map`, as
/// `vm_region_create_proxy` does: `len` bytes from the region's start in
/// its object, allowing at most `max_protection` and the region's.
///
/// # Errors
///
/// `NoSpace` if no entry holds `address`, `InvalidArgument` if `len` is
/// past the region's end or the range's end overflows, and as
/// `proxy::create`.
///
/// # Panics
///
/// If the region's object has no pager: not yet. As `Map::region`.
pub fn region_create_proxy(
    map: &Map,
    address: VirtualAddress,
    max_protection: Protection,
    len: usize,
) -> Result<Reference<Port>, Error> {
    let region = map.region(address)?;
    if len > region.end - region.start {
        return Err(Error::InvalidArgument);
    }
    let pager = match region.pager {
        Some(pager) => pager,
        // An internal object gets a pager now, which the default manager
        // takes.
        None => object::external::create_pager(&region.object)
            .unwrap_or_else(|| panic!("vm_object_pager_create: no pager, not yet")),
    };
    let start = (address - region.start) + usize::from(region.offset);
    let end = start.checked_add(len).ok_or(Error::InvalidArgument)?;
    Proxy::create(pager, max_protection & region.max_protection, start..end)
}

/// The default memory manager: the port new internal objects are given to,
/// held as a send right.
static DEFAULT_MANAGER: Spinlock<Option<Reference<Port>>> = Spinlock::new(None);

/// Sets the default memory manager to `manager`, which it keeps, as
/// `vm_set_default_memory_manager` does; gives the previous manager's send
/// right or, for none, a send right for the current one.
///
/// # Panics
///
/// If the current manager is dead, for none: not yet. As `Port::copy_send`.
pub fn set_default_memory_manager(manager: Option<SendRight>) -> Option<Reference<Port>> {
    let mut slot = DEFAULT_MANAGER.lock();
    let Some(manager) = manager else {
        return slot.as_ref().map(Port::copy_send);
    };
    slot.replace(manager.into_reference())
}

/// A send right for the default memory manager, if one is registered.
pub(in crate::vm) fn default_manager() -> Option<Reference<Port>> {
    DEFAULT_MANAGER.lock().as_ref().map(Port::copy_send)
}

/// The page cache statistics `vm_cache_statistics` reports, as GNU Mach
/// fills them: the external objects, and zero for the rest.
pub fn cache_statistics() -> [i32; 11] {
    let mut stats = [0; 11];
    stats[0] = i32::try_from(object::external::external_count()).unwrap();
    stats
}

/// The statistics `vm_statistics` reports, as GNU Mach fills them.
pub fn statistics() -> [i32; 13] {
    let (free, active, inactive, wired) = page::pool::statistics();
    let count = |count: &core::sync::atomic::AtomicU32| {
        i32::try_from(count.load(core::sync::atomic::Ordering::Relaxed)).unwrap()
    };
    [
        i32::try_from(PAGE_SIZE).unwrap(),
        i32::try_from(free).unwrap(),
        i32::try_from(active).unwrap(),
        i32::try_from(inactive).unwrap(),
        i32::try_from(wired).unwrap(),
        count(&super::STATISTICS.zero_fill_count),
        // No reactivations are counted yet.
        0,
        count(&super::STATISTICS.pageins),
        // No pageouts are counted yet.
        0,
        count(&super::STATISTICS.faults),
        count(&super::STATISTICS.cow_faults),
        count(&super::STATISTICS.lookups),
        count(&super::STATISTICS.hits),
    ]
}

/// Allocates `size` bytes of contiguous physical memory from `pmin` up to
/// `pmax` and maps them into `map` for reading and writing, as
/// `vm_allocate_contiguous` does; gives the address and the first page's
/// physical address.
///
/// # Errors
///
/// `InvalidArgument` if `pmin` is not 0, `palign` is not a page, or `size`
/// is 0 or rounds past the last address; `ResourceShortage` if no such
/// memory is free; as `Map::enter` and `Map::pageable`.
///
/// # Panics
///
/// As `object::init::allocate`, `Map::enter` and `Map::pageable`.
pub fn allocate_contiguous(
    map: &Map,
    size: usize,
    pmin: usize,
    pmax: usize,
    palign: usize,
) -> Result<(VirtualAddress, usize), Error> {
    if pmin != 0 {
        return Err(Error::InvalidArgument);
    }
    let palign = match palign {
        0 => PAGE_SIZE,
        palign if palign < PAGE_SIZE && PAGE_SIZE.is_multiple_of(palign) => PAGE_SIZE,
        palign => palign,
    };
    if palign != PAGE_SIZE {
        return Err(Error::InvalidArgument);
    }
    // vm_page_round.
    let size = size
        .checked_next_multiple_of(PAGE_SIZE)
        .ok_or(Error::InvalidArgument)?;
    if size == 0 {
        return Err(Error::InvalidArgument);
    }
    // The pages that fit below `pmax` once `size` is mapped.
    let first = pmin.div_ceil(PAGE_SIZE);
    let last = pmax.saturating_sub(size) / PAGE_SIZE + 1;
    let Some(pages) = page::alloc::allocate_contiguous(size / PAGE_SIZE, first, last) else {
        return Err(Error::ResourceShortage);
    };
    let object = object::init::allocate(size);
    let handle = Reference::clone(&object);
    let range = match map.enter(
        Placement::Anywhere,
        size,
        Some(object),
        Offset::from(0),
        Protection::READ | Protection::WRITE,
        Protection::READ | Protection::WRITE,
        Inheritance::Copy,
    ) {
        Ok(range) => range,
        Err(error) => {
            drop(handle);
            for page in pages {
                page::alloc::free(page);
            }
            return Err(error);
        }
    };
    {
        let mut state = handle.lock();
        for (index, page) in pages.iter().enumerate() {
            state.insert(&handle, page, index * PAGE_SIZE);
            page.wire();
        }
        drop(state);
    }
    if let Err(error) = map.pageable(
        range.clone(),
        Protection::READ | Protection::WRITE,
        RangeCheck::Checked,
    ) {
        drop(handle);
        deallocate(map, range.start, size);
        return Err(error);
    }
    for page in pages {
        page.unwire();
    }
    Ok((range.start, pages[0].address().0))
}

/// Copies `size` bytes of `map` from `source` over it at `destination`, as
/// `vm_copy` does.
///
/// # Errors
///
/// As `Map::copy_overwrite`.
///
/// # Panics
///
/// As `Map::copy_in`.
pub fn copy(
    map: &Map,
    source: VirtualAddress,
    size: usize,
    destination: VirtualAddress,
) -> Result<(), Error> {
    if size == 0 {
        return Ok(());
    }
    let copy = map.copy_in(source..source + size, Source::Keep);
    map.copy_overwrite(destination, Some(copy))
}

/// Wires `size` bytes of `map` from `address` for `access`, or unwires them
/// for none, as `vm_wire` does; an unprivileged `host` may wire at most 8MB.
///
/// # Errors
///
/// `InvalidHost` unless `host` is a host's port, alive, `InvalidTask` for
/// no map, `InvalidArgument` for access past reading, writing and running,
/// `NoAccess` if `host` is unprivileged and the map would hold more than
/// 8MB wired, and as `Map::pageable`.
///
/// # Panics
///
/// As `Map::pageable`.
pub fn wire(
    host: &Port,
    map: Option<&Map>,
    address: VirtualAddress,
    size: usize,
    access: u32,
) -> Result<(), Error> {
    let privileged = match host.kernel_object() {
        Some(KernelObject::Host(_)) => false,
        Some(KernelObject::HostPrivileged(_)) => true,
        _ => return Err(Error::InvalidHost),
    };
    let Some(map) = map else {
        return Err(Error::InvalidTask);
    };
    let access = Protection::from_bits(access).ok_or(Error::InvalidArgument)?;
    if !privileged && !access.is_empty() && map.size_wired().saturating_add(size) > WIRE_LIMIT {
        return Err(Error::NoAccess);
    }
    // As trunc_page and round_page.
    let start = address.align_down(PAGE_SIZE);
    let end =
        VirtualAddress::from(usize::from(address).wrapping_add(size)).next_multiple_of(PAGE_SIZE);
    map.pageable(start..end, access, RangeCheck::Checked)
}

/// Maps `size` bytes of `object` from `offset`, or with nothing behind them
/// yet for none, into `map`, as `placement` says, a fixed address rounded
/// down to a page and with no bit of `mask` set, `size` rounded up to
/// pages, as `Map::enter`, which takes `object`'s reference; returns where
/// they start.
///
/// # Errors
///
/// `InvalidArgument` if `size` rounds up past the last address, `NoSpace`
/// if a fixed start has a bit of `mask` set, and as `Map::enter`.
///
/// # Panics
///
/// For a mask with `Placement::Anywhere`: not yet. As `Map::enter`.
#[expect(clippy::too_many_arguments, reason = "vm_map_enter's parameters")]
fn enter(
    map: &Map,
    placement: Placement,
    size: usize,
    mask: usize,
    object: Option<Reference<Object>>,
    offset: Offset,
    protection: Protection,
    max_protection: Protection,
    inheritance: Inheritance,
) -> Result<VirtualAddress, Error> {
    let placement = match placement {
        Placement::Fixed(address) => Placement::Fixed(address.align_down(PAGE_SIZE)),
        Placement::Anywhere => Placement::Anywhere,
    };
    // Past the top, GNU Mach's rounding wraps to 0, which vm_map_enter refuses.
    let size = size
        .checked_next_multiple_of(PAGE_SIZE)
        .ok_or(Error::InvalidArgument)?;
    if let Placement::Fixed(start) = placement
        && usize::from(start) & mask != 0
    {
        return Err(Error::NoSpace);
    }
    assert!(
        mask == 0 || matches!(placement, Placement::Fixed(_)),
        "vm_map_find_entry_anywhere: a mask, not yet"
    );
    let range = map.enter(
        placement,
        size,
        object,
        offset,
        protection,
        max_protection,
        inheritance,
    )?;
    Ok(range.start)
}

/// Removes `size` bytes of `map` from `address`, rounded out to pages;
/// nothing for no bytes.
///
/// # Panics
///
/// As `Map::remove`.
pub fn deallocate(map: &Map, address: VirtualAddress, size: usize) {
    if size == 0 {
        return;
    }
    // GNU Mach's rounding, which wraps past the top.
    let end = usize::from(address)
        .wrapping_add(size)
        .wrapping_add(PAGE_SIZE - 1)
        & !(PAGE_SIZE - 1);
    map.remove(address.align_down(PAGE_SIZE)..VirtualAddress::from(end));
}

/// Writes `copy`'s memory over `map` from `address`; nothing for none.
///
/// # Errors
///
/// As `Map::copy_overwrite`.
///
/// # Panics
///
/// As `Map::copy_overwrite`.
pub fn write(
    map: &Map,
    address: VirtualAddress,
    copy: Option<CacheBox<MapCopy>>,
) -> Result<(), Error> {
    map.copy_overwrite(address, copy)
}

/// Sets what `size` bytes of `map` from `address`, rounded out to pages,
/// allow to `protection`, as `change` says.
///
/// # Errors
///
/// `InvalidArgument` if `protection` holds more than reading, writing,
/// running and `VM_PROT_NOTIFY`, and as `Map::protect`.
///
/// # Panics
///
/// If `protection` holds `VM_PROT_NOTIFY`: not yet. As `Map::protect`.
pub fn protect(
    map: &Map,
    address: VirtualAddress,
    size: usize,
    change: Change,
    protection: Protection,
) -> Result<(), Error> {
    /// `VM_PROT_NOTIFY`, which GNU Mach lets through.
    const NOTIFY: u32 = 0x10;
    if protection.bits() & !(Protection::all().bits() | NOTIFY) != 0 {
        return Err(Error::InvalidArgument);
    }
    assert!(
        Protection::all().contains(protection),
        "vm_protect: VM_PROT_NOTIFY, not yet"
    );
    // GNU Mach's rounding, which wraps past the top.
    let end = usize::from(address)
        .wrapping_add(size)
        .wrapping_add(PAGE_SIZE - 1)
        & !(PAGE_SIZE - 1);
    map.protect(
        address.align_down(PAGE_SIZE)..VirtualAddress::from(end),
        protection,
        change,
    )
}

/// Wires all of `map` as `flags` say, or unwires it all for none, if
/// `host` is the host's privileged port.
///
/// # Errors
///
/// `InvalidHost` unless `host` is the host's privileged port, alive,
/// `InvalidTask` for no map, `InvalidArgument` if `flags` hold a bit
/// `Wire` lacks, and as `Map::pageable_all`.
///
/// # Panics
///
/// As `Map::pageable_all`.
pub fn wire_all(host: &Port, map: Option<&Map>, flags: u32) -> Result<(), Error> {
    if !matches!(host.kernel_object(), Some(KernelObject::HostPrivileged(_))) {
        return Err(Error::InvalidHost);
    }
    let Some(map) = map else {
        return Err(Error::InvalidTask);
    };
    let Some(flags) = Wire::from_bits(flags) else {
        return Err(Error::InvalidArgument);
    };
    // Maps hold no projected buffers.
    map.pageable_all(flags)
}

/// Limits `map` to `current` bytes, at most `max` once raised, for `host`;
/// raising its most needs the host's privileged port.
///
/// # Errors
///
/// `InvalidArgument` if `current` is past `max`, `InvalidTask` for no map,
/// `InvalidHost` unless `host` is a host port, alive, and `NoAccess` if
/// `max` is past the map's most and `host` is not privileged.
pub fn set_size_limit(
    host: &Port,
    map: Option<&Map>,
    current: usize,
    max: usize,
) -> Result<(), Error> {
    if current > max {
        return Err(Error::InvalidArgument);
    }
    let Some(map) = map else {
        return Err(Error::InvalidTask);
    };
    let privileged = match host.kernel_object() {
        Some(KernelObject::Host(_)) => false,
        Some(KernelObject::HostPrivileged(_)) => true,
        _ => return Err(Error::InvalidHost),
    };
    let mut contents = map.lock();
    if max > contents.max_size_limit && !privileged {
        return Err(Error::NoAccess);
    }
    contents.size_limit = current;
    contents.max_size_limit = max;
    Ok(())
}

/// `map`'s size limit, and the most it may be raised to.
pub fn size_limits(map: &Map) -> (usize, usize) {
    let contents = map.contents.read();
    (contents.size_limit, contents.max_size_limit)
}
