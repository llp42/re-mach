// SPDX-License-Identifier: GPL-2.0-or-later
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira <leonardolopespereira@outlook.com>
// Inspired by GNU Mach; see COPYRIGHT.
//! ACPI: the root system description pointer (RSDP), and the tables
//! describing the APICs and the HPET.

use core::fmt::Write;
use core::ops::Range;
use core::{ptr, slice};

use super::address::PhysAddr;
use super::apic::{self, IoApic, MAX_IO_APICS, MAX_OVERRIDES, Override, Polarity, Trigger};
use super::mmio::Registers;
use super::per_cpu::PerCpu;
use super::serial::Serial;
use crate::boot::multiboot2::{Info, Tag};
use crate::collections::ArrayVec;
use crate::config::MAX_CPUS;
use crate::println;
use crate::smp::MachinePerCpu as _;
use crate::sync::OnceLock;

/// A verified copy of the RSDP, by revision.
enum Rsdp {
    /// ACPI 1.0's.
    V1([u8; Self::V1_SIZE]),
    /// ACPI >= 2.0's.
    V2([u8; Self::V2_SIZE]),
}

impl Rsdp {
    const SIGNATURE: &[u8; 8] = b"RSD PTR ";
    /// Its alignment in BIOS memory.
    const ALIGN: usize = 16;
    /// Offset of its revision byte.
    const REVISION: usize = 15;
    const V1_SIZE: usize = 20;
    const V2_SIZE: usize = 36;

    /// A copy of the RSDP at the start of `bytes`, if its signature,
    /// revision and checksum are good.
    fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.get(..Self::SIGNATURE.len())? != Self::SIGNATURE {
            return None;
        }
        let size = match *bytes.get(Self::REVISION)? {
            0 => {
                let _ = write!(Serial, "ACPI v1.0\r\n");
                Self::V1_SIZE
            }
            2 => {
                let _ = write!(Serial, "ACPI >= v2.0\r\n");
                Self::V2_SIZE
            }
            _ => return None,
        };
        let rsdp = bytes.get(..size)?;
        if rsdp.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)) != 0 {
            return None;
        }
        Some(if size == Self::V1_SIZE {
            Self::V1(rsdp.try_into().ok()?)
        } else {
            Self::V2(rsdp.try_into().ok()?)
        })
    }

    /// Searches the EBDA's first KiB, then the BIOS area at 0xe0000.
    ///
    /// # Safety
    ///
    /// The first 2 MiB are identity-mapped and readable.
    unsafe fn find_in_bios() -> Option<Self> {
        // SAFETY: the BIOS data area's EBDA segment; guaranteed by the
        // caller.
        let segment = unsafe { ptr::with_exposed_provenance::<u16>(0x40e).read() };
        let ebda = usize::from(segment) << 4;
        // SAFETY: guaranteed by the caller.
        unsafe { Self::find(ebda..ebda + 1024).or_else(|| Self::find(0xe_0000..0x10_0000)) }
    }

    /// The first RSDP in `range`, on 16-byte boundaries.
    ///
    /// # Safety
    ///
    /// `range`, and 36 bytes past it, are identity-mapped and readable.
    unsafe fn find(range: Range<usize>) -> Option<Self> {
        range
            .step_by(Self::ALIGN)
            .filter(|&address| address != 0)
            .find_map(|address| {
                let start = ptr::with_exposed_provenance::<u8>(address);
                // SAFETY: guaranteed by the caller.
                Self::parse(unsafe { slice::from_raw_parts(start, Self::V2_SIZE) })
            })
    }
}

/// Copy of the verified RSDP, once found.
static RSDP: OnceLock<Rsdp> = OnceLock::new();

/// The EFI system table's physical address, when booted through UEFI.
pub static EFI_SYSTEM_TABLE: OnceLock<u64> = OnceLock::new();

/// Saves the EFI system table address and a copy of the verified RSDP:
/// the boot information's newest, or else the BIOS's.
///
/// # Safety
///
/// Runs once, on the boot CPU. The first 2 MiB are identity-mapped and
/// readable.
pub unsafe fn init(boot_info: Info) {
    let (mut old, mut new) = (None, None);
    for tag in boot_info.tags() {
        match tag {
            Tag::AcpiOld(copy) => old = Some(copy),
            Tag::AcpiNew(copy) => new = Some(copy),
            Tag::Efi64(address) => {
                assert!(
                    EFI_SYSTEM_TABLE.set(address).is_ok(),
                    "acpi: two EFI system tables"
                );
            }
            _ => {}
        }
    }
    let rsdp = new
        .or(old)
        .and_then(Rsdp::parse)
        // SAFETY: guaranteed by the caller.
        .or_else(|| unsafe { Rsdp::find_in_bios() });
    if let Some(rsdp) = rsdp {
        assert!(RSDP.set(rsdp).is_ok(), "acpi: RSDP saved twice");
    }
}

/// Why the APIC tables could not be read.
#[derive(Debug)]
pub enum Error {
    /// No RSDP was found.
    NoRsdp,
    /// The RSDT or XSDT is not one.
    NoRsdt,
    /// A table does not sum to 0.
    BadChecksum,
    /// No MADT is listed.
    NoApic,
    /// The MADT lists no CPU or no IO APIC.
    ApicFailure,
    /// Registers could not be mapped.
    NoMapping,
}

/// A system description table, read through the direct map.
struct Table(&'static [u8]);

impl Table {
    /// Its header's size.
    const HEADER_SIZE: usize = 36;

    /// The table at `address`.
    fn at(address: PhysAddr) -> Self {
        let start = address.direct_map_ptr::<u8>();
        // SAFETY: ACPI tables are memory the firmware keeps, reached in
        // full through the direct map; the length is the header's.
        unsafe {
            let length =
                u32::from_le_bytes(slice::from_raw_parts(start.add(4), 4).try_into().unwrap());
            Self(slice::from_raw_parts(
                start,
                usize::try_from(length).unwrap(),
            ))
        }
    }

    fn signature(&self) -> &[u8] {
        &self.0[..4]
    }

    /// Whether its bytes sum to 0.
    fn is_valid(&self) -> bool {
        self.0.len() >= Self::HEADER_SIZE
            && self.0.iter().fold(0u8, |sum, &byte| sum.wrapping_add(byte)) == 0
    }

    /// Its `N` bytes at `offset`.
    fn bytes<const N: usize>(&self, offset: usize) -> [u8; N] {
        self.0[offset..offset + N].try_into().unwrap()
    }
}

/// The HPET's registers, once `init_apics` maps them.
pub static HPET: OnceLock<Registers> = OnceLock::new();

/// Size of the local APIC's and the HPET's register blocks.
const REGISTERS_SIZE: usize = 0x400;

/// Size of an IO APIC's register block: its select and window registers.
const IO_APIC_SIZE: usize = 0x20;

/// The IO APIC's version register, which holds its last input's number.
const IO_APIC_VERSION: u32 = 1;

/// Finds the CPUs, the local APIC, the IO APICs, their overrides and the
/// HPET in the ACPI tables, on the boot CPU, and maps their registers; the
/// boot CPU is numbered 0, the others in the firmware's order.
///
/// # Errors
///
/// If no table lists them, or a table is bad.
///
/// # Panics
///
/// On a second call.
pub fn init_apics() -> Result<(), Error> {
    let (root, wide) = root_table()?;
    let size = if wide { 8 } else { 4 };
    let count = (root.0.len() - Table::HEADER_SIZE) / size;
    let madt = find_tables(&root, wide, count)?;
    println!("ACPI:");
    println!(" rsdt/xsdt = {:#x} (n = {count})", root.0.as_ptr().addr());
    let local_apic = u32::from_le_bytes(madt.bytes(36));
    let local_apic = Registers::map(
        PhysAddr(usize::try_from(local_apic).unwrap()),
        REGISTERS_SIZE,
    )
    .map_err(|_| Error::NoMapping)?;
    let Madt {
        ids,
        io_apics,
        overrides,
    } = parse_madt(&madt)?;
    println!("CPUS:");
    for (cpu, id) in ids.iter().enumerate() {
        println!(
            " CPU {cpu} - APIC ID {id:x} - addr={:#x}",
            local_apic.address()
        );
    }
    println!("IOAPICS:");
    for (index, io_apic) in io_apics.iter().enumerate() {
        println!(
            " IOAPIC {index} - APIC ID {:x} - addr={:#x}",
            io_apic.id,
            io_apic.registers.address()
        );
    }
    assert!(
        apic::APIC_IDS.set(ids).is_ok()
            && apic::LOCAL_APIC.set(local_apic).is_ok()
            && apic::IO_APICS.set(io_apics).is_ok()
            && apic::OVERRIDES.set(overrides).is_ok(),
        "acpi: APICs found twice"
    );
    Ok(())
}

/// The RSDT, or the XSDT when the RSDP gives one, and whether it is the
/// XSDT, with 64-bit entries.
fn root_table() -> Result<(Table, bool), Error> {
    let (root, wide) = match RSDP.get().ok_or(Error::NoRsdp)? {
        Rsdp::V2(rsdp) if u64::from_le_bytes(rsdp[24..32].try_into().unwrap()) != 0 => {
            (u64::from_le_bytes(rsdp[24..32].try_into().unwrap()), true)
        }
        Rsdp::V1(rsdp) => (
            u64::from(u32::from_le_bytes(rsdp[16..20].try_into().unwrap())),
            false,
        ),
        Rsdp::V2(rsdp) => (
            u64::from(u32::from_le_bytes(rsdp[16..20].try_into().unwrap())),
            false,
        ),
    };
    let root = Table::at(PhysAddr(usize::try_from(root).unwrap()));
    if root.signature() != if wide { b"XSDT" } else { b"RSDT" } {
        return Err(Error::NoRsdt);
    }
    if !root.is_valid() {
        return Err(Error::BadChecksum);
    }
    Ok((root, wide))
}

/// Looks through the root table's `count` entries: maps the HPET's
/// registers, if listed, and returns the MADT.
fn find_tables(root: &Table, wide: bool, count: usize) -> Result<Table, Error> {
    let mut madt = None;
    for index in 0..count {
        let address = if wide {
            u64::from_le_bytes(root.bytes(Table::HEADER_SIZE + index * 8))
        } else {
            u64::from(u32::from_le_bytes(
                root.bytes(Table::HEADER_SIZE + index * 4),
            ))
        };
        let table = Table::at(PhysAddr(usize::try_from(address).unwrap()));
        match table.signature() {
            b"APIC" => madt = Some(table),
            b"HPET" => {
                // The base address in the table's generic address structure.
                let address = u64::from_le_bytes(table.bytes(44));
                let registers =
                    Registers::map(PhysAddr(usize::try_from(address).unwrap()), REGISTERS_SIZE)
                        .map_err(|_| Error::NoMapping)?;
                println!("HPET at physical address {address:#x}");
                assert!(HPET.set(registers).is_ok(), "acpi: HPET found twice");
            }
            _ => {}
        }
    }
    let madt = madt.ok_or(Error::NoApic)?;
    if !madt.is_valid() {
        return Err(Error::BadChecksum);
    }
    Ok(madt)
}

/// What the MADT lists.
struct Madt {
    /// The local APICs enabled or able to be, by CPU number.
    ids: ArrayVec<u8, MAX_CPUS>,
    /// The IO APICs, with their registers mapped.
    io_apics: ArrayVec<IoApic, MAX_IO_APICS>,
    /// The interrupt overrides.
    overrides: ArrayVec<Override, MAX_OVERRIDES>,
}

/// Reads what the MADT lists, as GNU Mach does.
fn parse_madt(madt: &Table) -> Result<Madt, Error> {
    let boot_id = PerCpu::current().apic_id();
    let (mut ids, mut io_apics, mut overrides) =
        (ArrayVec::new(), ArrayVec::new(), ArrayVec::new());
    ids.push(boot_id);
    let mut offset = 44;
    while offset + 2 <= madt.0.len() {
        let [kind, length] = madt.bytes(offset);
        let entry = &madt.0[offset..offset + usize::from(length).max(2)];
        let word = |at: usize| u32::from_le_bytes(entry[at..at + 4].try_into().unwrap());
        match kind {
            0 if word(4) & 3 != 0 && entry[3] != boot_id => {
                if ids.is_full() {
                    println!(
                        "acpi: CPU with APIC ID {:x} ignored: more than {MAX_CPUS} CPUs",
                        entry[3]
                    );
                } else {
                    ids.push(entry[3]);
                }
            }
            0 => {}
            1 => {
                let registers =
                    Registers::map(PhysAddr(usize::try_from(word(4)).unwrap()), IO_APIC_SIZE)
                        .map_err(|_| Error::NoMapping)?;
                registers.write(0, IO_APIC_VERSION);
                let inputs = (registers.read(0x10) >> 16 & 0xff) + 1;
                io_apics.push(IoApic {
                    id: entry[2],
                    gsi_base: word(8),
                    inputs,
                    registers,
                });
            }
            2 => {
                let flags = u16::from_le_bytes(entry[8..10].try_into().unwrap());
                overrides.push(Override {
                    irq: entry[3],
                    gsi: word(4),
                    polarity: match flags & 3 {
                        1 => Some(Polarity::High),
                        3 => Some(Polarity::Low),
                        _ => None,
                    },
                    trigger: match flags >> 2 & 3 {
                        1 => Some(Trigger::Edge),
                        3 => Some(Trigger::Level),
                        _ => None,
                    },
                });
            }
            _ => println!("Unhandled APIC entry type {kind:#x}"),
        }
        offset += usize::from(length).max(2);
    }
    if io_apics.is_empty() {
        return Err(Error::ApicFailure);
    }
    Ok(Madt {
        ids,
        io_apics,
        overrides,
    })
}
