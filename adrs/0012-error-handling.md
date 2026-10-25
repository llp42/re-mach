# Errors and failures

Distinguish recoverable errors, fatal boot failures, and violated
invariants, so failure handling is explicit and a missing optional resource
is never treated like corrupted kernel state.

- Return `Result` for recoverable failures; the caller chooses recovery.
  Use `Option` for expected absence.
- When a required boot resource is unavailable or invalid, stop
  initialization. Before halting, emit a diagnostic naming the failing
  stage and reason.
- Reserve `panic!` for violated invariants and conditions where continuing
  is unsafe. Document panic conditions on interfaces where relevant.
- Use `unwrap` or `expect` only where an established invariant guarantees
  the value, and explain that invariant locally. External input requires
  validation.
- Keep fatal errors and panic diagnostics in release builds; they must not
  depend on debug output.

Safety checks follow [ADR 0008](0008-unsafe-policy.md); detailed boot
output follows [ADR 0013](0013-boot-diagnostics.md).
