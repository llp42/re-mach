# Cargo build and assembly embedded in Rust

Use Cargo for kernel compilation and linking, with assembly embedded in
Rust. Keeping these steps in one build avoids a separate assembler or
linker workflow.

- `cargo build` alone produces the kernel artifact for the selected target,
  currently `kernel.efi`, a PE/COFF UEFI application.
- Use official, prebuilt targets available on stable. Targets requiring
  unstable compiler or Cargo features are deferred under
  [ADR 0002](0002-rust-stable.md).
- Use `rust-lld` as the linker.
- Image packaging and emulator tasks live in `mise.toml` and wrap the Cargo
  build.
- Embed assembly with `core::arch::asm!`, `global_asm!`, or `naked_asm!`.
  Use `options(att_syntax)` on x86/x86_64 and native syntax on other
  architectures. Keep assembly in Rust files rather than separate `.s`/`.S`
  files.
- Kernel dependencies must support `no_std` and the selected stable
  toolchain and target.
