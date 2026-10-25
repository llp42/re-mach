# Rust naming and documentation

The Rust implementation has its own interfaces and documentation. Name and
document each item by its role in this kernel, so readers understand its
contract without consulting an upstream implementation.

## Naming

- Follow Rust conventions: `snake_case` for files, modules, functions, and
  fields; `UpperCamelCase` for types and traits; `SCREAMING_SNAKE_CASE` for
  constants.
- Separate the words of compound names: `kernel_contracts.rs`,
  `BootStack`, `module_info`, `kernel_end`.
- Write `x86` for 32-bit and `x86_64` for 64-bit, in file names and prose.
- Keep an externally defined identifier where a toolchain, binary
  interface, or file format requires it.

## Comments and docs

- Document each item's purpose and contract: inputs, results, errors,
  ownership, lifetimes, and invariants, where relevant.
- Keep comments concise, objective, and technical; explain constraints and
  non-obvious choices instead of restating operations.
- Add module documentation where it explains the module's responsibility,
  boundaries, or use, at whatever length that needs; it is not limited to
  one line.
- Keep C names, C header paths, upstream equivalents, and porting
  attribution out of body comments and Rust documentation. Provenance
  belongs only in the licence header
  ([ADR 0011](0011-licensing-and-provenance.md)). The exception is the
  boundary with the temporary C base: code that calls, is called by, or
  builds that C may name the C symbols and files involved.
- Document unsafe contracts and local safety justifications as
  [ADR 0008](0008-unsafe-policy.md) requires.
