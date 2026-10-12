# Boot information as a blob of structures

Use a blob of typed, length-delimited structures for the boot handoff,
following the NetBSD-style boot information list being implemented in
`src/loader.rs` and `src/arch/x86_64/boot_info.rs`. Records let boot paths
provide different information through one handoff representation.

This selects the representation style, not a frozen binary format or a
promise of compatibility with another kernel's boot ABI. Define record
layouts, bounds, alignment, address meaning, and backing-memory lifetime
as the contract develops. Consumers must validate records before accessing
their payloads; a structure cast alone does not establish validity.

Firmware-specific records may carry raw firmware information. Decode them
at the boot or firmware boundary before shared subsystems consume it.
Architecture-specific records belong to their architecture implementation.
The blob is an internal boot contract, separate from the GNU Mach user ABI.

Unlike NetBSD's x86 UEFI boot, the kernel image is itself the UEFI
application and builds the blob; no separate loader runs first. UEFI is the
only boot path for now, and a Multiboot path will come later. The kernel is
64-bit only and is entered in long mode. NetBSD's loader instead drops to
32-bit protected mode, because NetBSD's amd64 kernel entry is 32-bit code
shared with its BIOS boot. The kernel reads the records where the boot path
stored them and does not copy them into kernel memory as NetBSD's kernel
entry does, so that memory stays reserved while the records are in use.

This refines [ADR 0005](0005-uefi-first.md); the existing x86_64 location and
UEFI dependencies still need incremental separation.
