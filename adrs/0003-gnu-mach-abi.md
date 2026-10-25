# GNU Mach ABI, new internals

GNU Mach's public ABI is the compatibility target for user space. Design
the kernel internals anew in Rust; compatibility does not require GNU
Mach's internal structures, module boundaries, or algorithms. Keeping
only the external contract lets the internals evolve independently. The
internals in use are DragonFly BSD's transitional base
([ADR 0015](0015-dragonfly-c-base.md)), designed anew as Rust replaces them.

- Compatibility covers the binary representation and observable semantics
  of each supported interface, including calls, messages, types, and return
  values. Internal layouts are not implicitly part of the ABI; translate
  boundary representations into internal Rust types where needed.
- Each implemented interface must identify its GNU Mach reference revision
  and target ABI, with compatibility checks. This decision selects no
  revision and claims no complete compatibility.
- Additional architectures must define their ABI explicitly rather than
  inherit x86_64 layout assumptions.

[ADR 0006](0006-validation-milestones.md) defines the compatibility baseline
and current boot milestone.
