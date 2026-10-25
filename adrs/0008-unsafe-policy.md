# Minimal unsafe regions with explicit invariants

Use safe Rust by default. Use `unsafe` only where required, or for a
performance improvement supported by measurement; both motivations carry
the same safety requirements. Keep each unsafe region as small as possible
and isolate it behind an interface whose callers can meet its safety
requirements.

- State the preconditions of unsafe interfaces, and explain why they hold
  at each unsafe operation or implementation.
- Check safety conditions that can be checked safely at runtime with
  `debug_assert!`, close to and before the unsafe operation.
- For conditions that cannot be checked this way, document how ownership,
  lifetimes, synchronization, or the caller's contract establishes them. A
  placeholder assertion is not a check.
- Safety must hold with debug assertions disabled.
  [`debug_assert!`](https://doc.rust-lang.org/core/macro.debug_assert.html)
  is off in optimized builds by default and only detects invariant
  violations during development, so it is never the sole enforcement of a
  safety precondition. Conditions not already guaranteed by the interface
  or implementation need validation that remains in release builds, such
  as checked operations, error handling, or `assert!`, before the unsafe
  operation.
- Assertions must not perform an invalid access or contain side effects
  needed for correctness.
