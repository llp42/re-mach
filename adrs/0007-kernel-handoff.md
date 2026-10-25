# Kernel handoff

The boot code enters the imported kernel
([ADR 0015](0015-dragonfly-c-base.md)) with the handoff that DragonFly
BSD's loader gives its pc64 kernel, because `btext` and `hammer_time()`
consume it; matching it leaves the imported C unchanged. The handoff is an
internal boot contract, separate from the GNU Mach user ABI, and is
redefined when Rust replaces the kernel's entry and early initialization.
This refines [ADR 0005](0005-multiboot-first.md).

The Multiboot loader loads the kernel file at physical `0x200000` through
the header's address fields, which accept a 64-bit image, and zeroes it up
to the image's end. The image is linked so that each loadable segment
belongs at `0x200000` plus its file offset; the boot code verifies this
before relying on it. The entry code switches to long mode under static
page tables that map physical memory up to 1 GiB both at the same
addresses and at `KERNBASE`.

The handoff consists of:

- the kernel image in place, as loaded;
- the init program, the first Multiboot module, where the loader placed it
  above the kernel image;
- the kernel environment, above everything the loader placed:
  NUL-terminated `name=value` strings, ended by an empty string. It names
  devfs, which needs no disk, as the root filesystem
  (`vfs.root.mountfrom=devfs:`);
- the preload metadata at `modulep`, after the environment: records of a
  `u32` type, a `u32` size and `size` bytes of data, each starting 8-byte
  aligned, ended by a record of type and size zero. The kernel's records
  are its name, type `elf kernel`, physical address and size, boot flags,
  environment address, and the loader's memory map as packed base, length
  and type entries. A second module follows with name `/boot/init`, type
  `init`, physical address and size;
- `kernend`, the 4 KiB-aligned end of the metadata, below 1 GiB, followed
  by usable memory that the kernel's first page tables overwrite;
- the entry page tables, and a stack holding `modulep` and `kernend` where
  `btext` reads them.

The kernel retains the memory from `0x200000` to its first free page,
which holds the image, the init program, the environment and the metadata.
Only records the kernel reads are emitted. Before handing over, the boot
code checks the loader's structures and the memory it writes against the
loader's memory map. The init program is passed as opaque bytes; the
kernel validates it as an ELF executable and runs it as the first user
process instead of executing `/sbin/init`.
