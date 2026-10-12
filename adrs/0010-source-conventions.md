# Rust naming and documentation

The Rust implementation has its own interfaces and documentation. Name and
document items according to their role in this kernel, so readers can
understand their contracts without consulting an upstream implementation.

## Naming

- Use Rust naming conventions: `snake_case` for files, modules, functions,
  and fields; `UpperCamelCase` for types and traits; `SCREAMING_SNAKE_CASE`
  for constants.
- Separate words in compound names: `md_global_data.rs` / `MdGlobalData`,
  `private_space.rs` / `PrivateSpace`, `boot_info.rs` / `BootInfo`,
  `kernel_end`.
- For the x86 family, use `x86` (32-bit) and `x86_64` (64-bit) in file names
  and prose.
- Preserve externally defined identifiers where required by a toolchain,
  binary interface, or file format.

## Comments and docs

- Document the Rust item's purpose and contract: inputs, results, errors,
  ownership, lifetimes, and invariants where relevant.
- Keep comments concise, objective, and technical. Explain constraints and
  non-obvious choices rather than restating operations.
- Use module documentation when it helps explain the module's responsibility,
  boundaries, or use. Its length follows the information needed; there is
  no one-line restriction.
- Keep C names, C header paths, upstream equivalents, and porting attribution
  out of body comments and Rust documentation. Source provenance belongs
  exclusively in the licence header under
  [ADR 0011](0011-licensing-and-provenance.md).
- Document unsafe contracts and local safety justifications according to
  [ADR 0008](0008-unsafe-policy.md).
