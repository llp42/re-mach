# Cargo build and assembly embedded in Rust

Use Cargo to compile and link the kernel, with new assembly embedded in
Rust, so the build needs no separate make, assembler, or linker workflow.

- `cargo build` alone produces the bootable artifact for the selected
  target: the kernel, an ELF executable for `x86_64-unknown-none` that a
  Multiboot loader boots directly ([ADR 0005](0005-multiboot-first.md)).
- The root crate is the kernel. Its build script gives rustc the link
  arguments and, while the C base exists, compiles it
  ([ADR 0015](0015-dragonfly-c-base.md)).
- Each architecture has its own linker script in `link/`, currently
  `link/x86_64.ld`, derived from the imported one. Linker scripts are build
  configuration and stay outside `src/`. The script places the Multiboot
  header in the file's first 8 KiB and marks where the loaded file
  contents end.
- The build script also builds the `tests/init/` program, which is passed
  to the kernel as a Multiboot module: a static executable for
  `x86_64-unknown-none` at a fixed user address, built by a nested Cargo
  invocation with its own target directory, because stable Cargo cannot
  build an executable with other code generation inside the same crate.
  The build script refuses it if it has dynamic relocations.
- Emulator tasks live in `mise.toml` and wrap the Cargo build. They refuse
  a kernel with dynamic relocations, since a build script cannot inspect
  its own crate's linked executable.
- Use official, prebuilt targets available on stable; defer targets that
  need unstable compiler or Cargo features
  ([ADR 0002](0002-rust-stable.md)).
- Link all artifacts with `rust-lld`.
- Embed new assembly in Rust files with `core::arch::asm!`, `global_asm!`,
  or `naked_asm!`, using `options(att_syntax)` on x86/x86_64 and native
  syntax on other architectures. Separate assembly files exist only in the
  imported C.
- Kernel dependencies must support `no_std` and the selected stable
  toolchain and target. Build dependencies run on the host.
