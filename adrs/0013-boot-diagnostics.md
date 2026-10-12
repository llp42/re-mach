# Boot diagnostics

Use serial output to identify boot progress and failures independently of
the graphical console. Detailed development output must not impose runtime
work on release builds.

- Provide `dbg_println!()` for formatted diagnostic lines. In debug builds,
  include the calling module, file, and line with the message.
- Enable the macro with `cfg(debug_assertions)`. Under the repository's
  release profile it expands to no operation: no argument evaluation,
  formatting, or output. Arguments must not contain required side effects.
- Keep the diagnostic interface independent of firmware services. The current
  backend is the early serial writer; output requires its initialization.
- Identify initialization stages and relevant values, such as image location,
  memory-map size, and boot information availability.
- Emit `re-mach: boot complete` only after the implemented initialization
  sequence completes. Keep this marker, fatal errors, and panic output in both
  debug and release. The marker does not imply user-space or ABI support.
- Capture serial output during boot validation. Record the build profile,
  emulator, firmware, command, and result. Use a bounded wait for the success
  marker; reaching the intended idle loop is success, while timing out before
  the marker is a failure.

The early writer does not establish a concurrency or interrupt-safe logging
contract. Define that contract when those execution contexts are introduced.
