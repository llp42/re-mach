# Minimal unsafe regions with explicit invariants

Use safe Rust by default. Use `unsafe` only where required, or for a
performance improvement supported by measurement. Keep each unsafe region
as small as possible and isolate it behind an interface whose callers can
meet its safety requirements. A performance motivation has the same safety
requirements as any other unsafe operation.

State preconditions on unsafe interfaces and explain why they hold at each
unsafe operation or implementation. Associate unsafe operations with
`debug_assert!` checks for safety conditions that can be checked safely at
runtime. Keep checks close to the operation and before it. For conditions
that cannot be checked this way, document how ownership, lifetimes,
synchronization, or the caller's contract establishes them; a placeholder
assertion is not a check.

Safety must hold when debug assertions are disabled. Conditions not already
guaranteed by the interface or implementation require release-mode
validation, such as checked operations, error handling, or `assert!`, before
the unsafe operation. Assertions must not themselves perform an invalid
access or contain side effects needed for correctness.

[`debug_assert!`](https://doc.rust-lang.org/core/macro.debug_assert.html)
is disabled in optimized builds by default; it detects invariant violations
during development and is not the sole enforcement of a safety precondition.
