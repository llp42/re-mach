# Architecture decisions

Every ADR here is valid; together they are the authoritative source of
development rules. An ADR sets the required direction without claiming
that implementation or compatibility is complete.

The ADRs govern the Rust code and the Cargo build around it. The imported
DragonFly BSD C, its glue, and the steps that compile them are temporary
until the kernel is fully migrated to Rust; only
[ADR 0015](0015-dragonfly-c-base.md) governs them.

| ADR | Decision |
| --- | --- |
| [0001](0001-rust.md) | Rust implementation |
| [0002](0002-rust-stable.md) | Rust Stable |
| [0003](0003-gnu-mach-abi.md) | GNU Mach ABI, new internals |
| [0004](0004-x86_64-first.md) | x86_64 first, architecture isolation |
| [0005](0005-multiboot-first.md) | Multiboot first, boot isolation |
| [0006](0006-validation-milestones.md) | Validation milestones |
| [0007](0007-kernel-handoff.md) | Kernel handoff |
| [0008](0008-unsafe-policy.md) | Minimal unsafe regions with explicit invariants |
| [0009](0009-build-and-assembly.md) | Cargo build and assembly embedded in Rust |
| [0010](0010-source-conventions.md) | Rust naming and documentation |
| [0011](0011-licensing-and-provenance.md) | Licensing and provenance |
| [0012](0012-error-handling.md) | Errors and failures |
| [0013](0013-boot-diagnostics.md) | Boot diagnostics |
| [0014](0014-change-discipline.md) | Reviewable changes |
| [0015](0015-dragonfly-c-base.md) | DragonFly BSD C base, replaced by Rust |

Read before a change:

- code: ADRs 0001, 0002, 0006, 0008, 0010–0012, 0014, and 0015;
- build or assembly: also ADR 0009;
- ABI, architecture, or boot: whichever of ADRs 0003–0005 and 0007
  apply;
- boot or diagnostics: also ADR 0013.

## Pending design

The memory, address, concurrency, and interrupt models in use come from
the imported DragonFly BSD kernel ([ADR 0015](0015-dragonfly-c-base.md)).
Rules for their Rust replacements are not chosen yet; record them when
their contracts are defined. Running the imported code does not imply
keeping all upstream internals.

## Recording decisions

Name a new ADR with the next sequential `NNNN-short-title.md`. Record
context, decision, and rationale; add consequences only where useful. Use
no status field or lifecycle labels.

When a decision changes, edit its ADR. Delete a canceled ADR; its old
contents live only in Git history. Update this index and affected
references in the same change. Keep unresolved proposals out of ADR files.
