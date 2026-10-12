# Validation milestones

The current milestone is to compile and boot without errors. Establish a
working kernel entry and initialization path before requiring user-space
execution or GNU Mach compatibility tests.

For Rust changes, run `cargo fmt --check`, `cargo build`, and
`cargo build --release` with the repository toolchain. For boot or hardware
initialization changes, also boot both profiles in QEMU: verify progress
through firmware exit and kernel initialization, reaching the intended idle
state without panic, exception, or unexpected reset. The interactive debug
boot task is `mise run qemu`; an equivalent headless invocation may be used
for automated validation. An intentional idle loop is not a boot failure.
Capture the success marker and diagnostics specified in
[ADR 0013](0013-boot-diagnostics.md). Report commands, observed results, and
checks that could not run.

The first compatibility milestone is to pass the existing GNU Mach tests.
When integrating them, record the upstream revision, test inventory, and
execution procedure; report unsupported or skipped tests explicitly.
This establishes a reproducible baseline without inventing a replacement
suite as the initial compatibility target.

Later, execute user-space programs to validate the public ABI in use.
Layout checks and focused tests support this work but do not replace program
execution. Passing the boot milestone alone makes no ABI compatibility claim.

This refines the validation scope of [ADR 0003](0003-gnu-mach-abi.md).
