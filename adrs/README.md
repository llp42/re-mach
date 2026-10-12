# Architecture decisions

Every ADR present in this directory is valid and defines development
constraints. This directory is the authoritative source for development
rules. A decision records the required direction; its presence does not
claim that implementation or compatibility is complete.

| ADR | Decision |
| --- | --- |
| [0001](0001-rust.md) | Rust implementation |
| [0002](0002-rust-stable.md) | Rust Stable |
| [0003](0003-gnu-mach-abi.md) | GNU Mach ABI, new internals |
| [0004](0004-x86_64-first.md) | x86_64 first, architecture isolation |
| [0005](0005-uefi-first.md) | UEFI first, boot isolation |
| [0006](0006-validation-milestones.md) | Boot first, GNU Mach tests, then programs |
| [0007](0007-boot-information-blob.md) | Boot information as a blob of structures |
| [0008](0008-unsafe-policy.md) | Minimal unsafe regions and safety checks |
| [0009](0009-build-and-assembly.md) | Cargo build and assembly embedded in Rust |
| [0010](0010-source-conventions.md) | Rust naming and documentation |
| [0011](0011-licensing-and-provenance.md) | Licensing and provenance |
| [0012](0012-error-handling.md) | Errors and failures |
| [0013](0013-boot-diagnostics.md) | Boot diagnostics |
| [0014](0014-change-discipline.md) | Reviewable changes |

For code changes, read ADRs 0001, 0002, 0006, 0008, 0010–0012, and 0014.
For build or assembly changes, also read ADR 0009. ABI, architecture, and boot
changes require the relevant ADRs 0003–0005 and 0007; boot and diagnostic
changes also require ADR 0013.

## Pending design

The memory and address model is undecided; DragonFly BSD is the likely
design reference. The concurrency and interrupt model will draw from
DragonFly BSD, but its concrete rules have not been selected. Record those
decisions when their contracts are defined; these directions do not imply
adoption of all upstream internals.

## Recording decisions

Use the next sequential `NNNN-short-title.md` filename. Record context,
decision, and rationale; add consequences only where useful. ADRs have no
status field or lifecycle labels.

Edit an ADR when its decision changes. Delete a canceled ADR; its previous
contents remain only in Git history. Update this index and affected
references in the same change. Keep unresolved proposals outside ADR files.
