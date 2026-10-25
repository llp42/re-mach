# Validation milestones

Milestones set the validation scope, refining
[ADR 0003](0003-gnu-mach-abi.md). The current milestone is to compile, boot
without errors, and run a first user-space program: a working kernel entry,
initialization path, and user-mode execution come before GNU Mach
compatibility tests are required.

Validate changes as follows; report the commands, observed results, and
checks that could not run.

- Rust changes: `cargo fmt --check`, `cargo build`, and
  `cargo build --release` with the repository toolchain. The root build
  also builds the `tests/init/` program and refuses it with dynamic
  relocations; the emulator tasks refuse a kernel with them
  ([ADR 0009](0009-build-and-assembly.md)).
- `tools/cbuild/` or `tests/init/` changes: also `cargo fmt --check` with
  their `--manifest-path`.
- Boot, hardware initialization, imported C, kernel build, or user-program
  changes: also boot both profiles in QEMU through Multiboot (`-kernel`,
  with the init program as `-initrd`), capturing the markers and
  diagnostics of [ADR 0013](0013-boot-diagnostics.md). Verify progress
  through the boot code, kernel entry, kernel initialization, and the init
  program's output, without a trap, unexpected reset, or panic other than
  the intended end state: the init program exits after its output, and the
  kernel reports `init died (signal 0, exit 0)` and panics with
  `Going nowhere without my init!`.
- `mise run qemu` is the interactive debug boot task; automated validation
  may use an equivalent headless invocation.

The first compatibility milestone is to pass the existing GNU Mach tests, a
reproducible baseline instead of a newly invented suite. When integrating
them, record the upstream revision, test inventory, and execution
procedure, and report unsupported or skipped tests explicitly.

Later, execute user-space programs to validate the public ABI in use;
layout checks and focused tests support but do not replace program
execution. Passing the boot milestone alone makes no ABI compatibility
claim.
