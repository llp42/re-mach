# GNU Mach ABI, new internals

GNU Mach's public ABI is the compatibility target for user space. Design
the kernel internals anew in Rust; compatibility does not require preserving
the original implementation's internal structures, module boundaries, or
algorithms. This preserves the external contract while allowing independent
internal evolution.

Compatibility covers the binary representation and observable semantics of
each supported interface, including calls, messages, types, and return
values. Translate boundary representations into internal Rust types where
needed. Internal layouts are not implicitly part of the ABI.

This decision does not select a GNU Mach reference revision or claim complete
compatibility. Each implemented interface must identify its reference
revision and target ABI, with compatibility checks. Additional architectures
must define their ABI explicitly rather than inherit x86_64 layout assumptions.

The compatibility baseline and current boot milestone are defined in
[ADR 0006](0006-validation-milestones.md).
