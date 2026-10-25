# Rust Stable

Build the kernel with the stable Rust toolchain selected by
`rust-toolchain.toml`, so the build does not depend on unstable toolchain
behavior. Language features, dependencies, and targets must work with the
stable compiler and Cargo.

- Nightly, unstable feature gates, `RUSTC_BOOTSTRAP`, and `-Z` options such
  as `build-std` are outside the supported build.
- If a feature or target needs them, use a stable alternative or defer it.
- Changing this constraint requires updating this ADR in the same change.
