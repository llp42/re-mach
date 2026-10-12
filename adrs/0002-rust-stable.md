# Rust Stable

Build the kernel with the stable Rust toolchain selected by
`rust-toolchain.toml`. Stable compiler and Cargo support is a requirement
for language features, dependencies, and targets, keeping the build
independent of unstable toolchain behavior.

Nightly, unstable feature gates, `RUSTC_BOOTSTRAP`, and `-Z` options such as
`build-std` are outside the supported build. If a feature or target requires
them, use a stable alternative or defer it. Changing this constraint requires
updating this ADR in the same change.
