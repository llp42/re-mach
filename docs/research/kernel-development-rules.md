# Kernel development rules: research

Research checked on 2026-10-13. This note compares upstream guidance and
suggests possible refinements for re-mach. It defines no project rules;
[ADRs](../../adrs/README.md) remain authoritative.

## Upstream guidance

- FreeBSD requires testing before committing, including kernel builds and
  affected supported architectures. It also separates formatting changes
  from functionality changes to keep reviews and history understandable.
  [Committer's Guide, sections 20.2 and 20.4](https://docs.freebsd.org/en/articles/committers-guide/)
- FreeBSD provides `INVARIANTS` for runtime checks of kernel structures and
  algorithms. `WITNESS` checks lock acquisition order for potential deadlocks.
  These are diagnostic facilities with runtime costs, not a substitute for
  correct implementation.
  [Kernel Debugging](https://docs.freebsd.org/en/books/developers-handbook/kerneldebug/)
- Linux's Rust guidelines use `rustfmt`, distinguish unsafe interface
  contracts (`# Safety`) from explanations at unsafe operations (`// SAFETY:`),
  and prefer fallible operations, typically returning `Result`, over panics.
  [Rust Coding Guidelines](https://www.kernel.org/doc/html/latest/rust/coding-guidelines.html)
- The same Linux guidelines prefer `#[expect(...)]` over `#[allow(...)]`
  because obsolete expectations produce diagnostics. Exceptions include
  conditional compilation, macro expansion, and architecture-dependent
  warnings.
  [Rust lint guidelines](https://www.kernel.org/doc/html/latest/rust/coding-guidelines.html#lints)
- DragonFly's introductory kernel guide recommends addressing compiler
  warnings, reusing existing facilities, and seeking code review. It also
  suggests message passing as an alternative to locking where appropriate;
  this is guidance, not a universal prohibition on locks.
  [Kernel developer guide](https://www.dragonflybsd.org/docs/howtos/howtonewkerneldeveloper/)

## Recommendations for re-mach

The following are recommendations derived from those practices, not upstream
requirements or new decisions:

1. **Make boot evidence repeatable.** Refine [ADR 0006](../../adrs/0006-validation-milestones.md)
   with a defined success marker, serial log capture, and a timeout that
   distinguishes a successful idle loop from a hang before initialization.
   Record the emulator and firmware used. Keep the current build-and-boot
   milestone; user-space tests remain later work.
2. **Define failure behavior.** Distinguish recoverable errors from violated
   invariants and fatal boot failures. Prefer `Result` for recoverable
   failures; document when boot must stop and what diagnostic it emits.
3. **Keep warnings actionable.** Address new warnings and keep justified
   suppressions local. Consider `#[expect(...)]` where configuration permits.
   Preserve automatic formatting already required by ADR 0006.
4. **Keep changes reviewable.** Separate mechanical formatting from behavior
   changes and describe the reason, observable result, and validation in
   commit messages. Focus additional review on unsafe and boot boundaries.
5. **Preserve useful diagnostics.** Extend existing boot output to identify
   initialization stages and invariant failures. The safety contracts and
   release-mode obligations already in [ADR 0008](../../adrs/0008-unsafe-policy.md)
   remain essential; debug assertions alone do not establish soundness.

Defer specific lock ordering, interrupt contexts, allocation restrictions,
and message-passing rules until the concurrency and memory designs exist.
The upstream diagnostic and design examples help frame those later choices
without selecting their implementation now.
