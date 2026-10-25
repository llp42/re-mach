# Boot diagnostics

Boot progress and failures are reported on serial output, independently of
the graphical console. Detailed development output must not impose runtime
work on release builds.

- `dbg_println!()` prints a formatted diagnostic line. It is enabled by
  `cfg(debug_assertions)`, and then includes the calling module, file, and
  line. Under the repository's release profile it expands to nothing: no
  argument evaluation, formatting, or output, so arguments must not carry
  required side effects.
- The diagnostic interface is independent of firmware services. Its current
  backend is the early serial writer, which must be initialized first.
- Diagnostics identify initialization stages and relevant values, such as
  kernel image placement, memory-map size, and handoff addresses.
- The boot code prints `re-mach: multiboot entry` once the early writer is
  ready, and `re-mach: entering kernel` after writing the handoff,
  immediately before jumping to the imported kernel. From then on, the
  kernel's own console replaces the early writer.
- Before the kernel's console exists, a Rust panic would end without
  output, so the boot code avoids operations that panic and reports
  failures with the early writer's fatal error.
- The kernel crate's initialization hook prints
  `re-mach: kernel Rust initialized` through the kernel console.
- The init program writes `re-mach: hello, world` to the console and exits;
  the kernel then reports `init died (signal 0, exit 0)` and panics with
  `Going nowhere without my init!`, the intended end state.
- These markers, like fatal errors and panic output
  ([ADR 0012](0012-error-handling.md)), stay in both debug and release
  builds. They do not imply GNU Mach ABI support.
- Boot validation ([ADR 0006](0006-validation-milestones.md)) captures
  serial output and records the build profile, emulator, firmware, command,
  and result. It waits a bounded time for the success markers: reaching the
  end state after the init program's marker is success; timing out before
  it is failure.

The early writer provides no concurrency or interrupt-safe logging
contract; define one when those execution contexts are introduced.
