# Multiboot first, boot isolation

Multiboot is the first supported boot protocol. The kernel image is the
bootable artifact: a Multiboot loader, such as GRUB or QEMU's `-kernel`,
loads the file and enters it, with no separate loader program. This keeps
one compilation and one artifact for a kernel whose imported C must run as
an ELF executable at `KERNBASE` ([ADR 0015](0015-dragonfly-c-base.md)),
and matches how GNU Mach and its tests boot.

- The kernel carries the Multiboot header and the entry code.
- Boot-specific code translates the loader's information into the kernel
  handoff ([ADR 0007](0007-kernel-handoff.md)) and keeps the protocol's
  structures out of shared subsystems.
- Define the ownership and lifetime of the supplied memory and modules.
- Boot protocol and CPU architecture remain independently selectable: a
  new boot path, such as UEFI, supplies the same handoff without
  duplicating kernel logic. Until a UEFI path exists, UEFI machines boot
  the kernel through a Multiboot loader.
