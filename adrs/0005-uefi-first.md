# UEFI first, boot isolation

UEFI is the first supported boot environment. It provides the initial
loading and firmware integration path, while the kernel's shared
initialization must remain usable by future boot paths.

Boot-specific code collects and translates firmware information into an
explicit kernel-owned handoff contract. Define the ownership and lifetime
of the supplied memory and resources. UEFI handles, protocols, and raw
memory descriptors stay behind this boundary; shared subsystems consume
the information they need without depending on UEFI services.

UEFI Boot Services end before control passes to shared kernel initialization.
Any future use of Runtime Services requires an explicit firmware interface.
Boot protocol and CPU architecture remain independently selectable; a new
boot path supplies the same kernel handoff without duplicating shared logic.

The handoff representation is defined in
[ADR 0007](0007-boot-information-blob.md).
