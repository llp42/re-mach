# Errors and failures

Distinguish recoverable errors, fatal boot failures, and violated invariants.
This makes failure handling explicit and prevents a missing optional resource
from being treated like a corrupted kernel state.

- Return `Result` for recoverable failures and let the caller choose recovery.
  Use `Option` for expected absence, such as an optional framebuffer.
- Stop initialization when a required boot resource is unavailable or invalid.
  Emit a diagnostic identifying the failing stage and reason before halting.
- Reserve `panic!` for violated invariants or conditions where continuing is
  unsafe. Document panic conditions on interfaces where relevant.
- Use `unwrap` or `expect` only when an established invariant guarantees the
  value; explain that invariant locally. External input requires validation.
- Fatal errors and panic diagnostics remain available in release builds;
  they must not depend on debug output.

Safety checks follow [ADR 0008](0008-unsafe-policy.md). Detailed boot output
follows [ADR 0013](0013-boot-diagnostics.md).
