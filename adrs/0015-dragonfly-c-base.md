# DragonFly BSD C base, replaced by Rust

The kernel starts from DragonFly BSD's pc64 kernel in C, configured as
DFLYMINI and imported under `dfly/` at the upstream commit recorded in
`dfly/UPSTREAM`. A complete, booting kernel lets each Rust replacement run
in a working system instead of requiring every subsystem before anything
executes. The C is temporary: it only shrinks, and it goes away once the
kernel is fully migrated to Rust. This is the only ADR that governs it;
the others govern the Rust code.

- Rust replaces the C one function or subsystem at a time; each
  replacement removes the C it replaces and follows the Rust rules,
  provenance included ([ADR 0011](0011-licensing-and-provenance.md)).
- `dfly/` holds upstream files copied verbatim, with their own notices and
  no SPDX header. They change only to remove or redirect code that Rust
  replaces. `dfly/UPSTREAM` records the repository, commit, imported
  paths, changed files, and the files generated from the configuration
  (`conf/DFLYMINI`, `conf/kernel/`, `files.txt`); `dfly/COPYRIGHT` is the
  upstream copyright file.
- New C is limited to glue connecting Rust to the imported code, such as
  `src/glue.c`. Glue may adapt upstream code. Non-Rust files written here,
  such as the glue and the kernel linker script, carry the
  [ADR 0011](0011-licensing-and-provenance.md) header lines in their own
  comment syntax.
- The root crate's build script compiles the imported C, its assembly, and
  the glue with the host `gcc`, generates upstream's derived sources with
  `awk` and `nm`, and links them with the crate
  ([ADR 0009](0009-build-and-assembly.md)). This needs gcc, awk, and
  binutils (`nm`, `readelf`) on the host. DragonFly's own loader and build
  system are not imported.
- The kernel boots directly through Multiboot
  ([ADR 0005](0005-multiboot-first.md)); its Rust boot code enters the
  imported kernel at `btext` with the
  [ADR 0007](0007-kernel-handoff.md) handoff.
- When no C remains, `dfly/`, the glue, and the C build steps are removed.
- The GNU Mach ABI remains the user-space target
  ([ADR 0003](0003-gnu-mach-abi.md)); the imported DragonFly internals are
  a starting point, not a compatibility commitment.
