# Licensing and provenance

New code uses MIT; ported code retains its source licences. Keep attribution
and upstream references in the source header so provenance remains traceable
without dictating the Rust implementation's documentation.

## Licence header

Every source file opens with its licences, then its copyright holders, one
line each. The header sits above the `//!` module doc, so line 1 of every
`.rs` file is `// SPDX-License-Identifier:`.

Ours, on a file written here:

    // SPDX-License-Identifier: MIT
    // SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira

A file carrying ported code takes its sources' licence, not MIT: its own
line is a copyright only, and each source's licence and copyright lines sit
in that source's provenance block:

    // SPDX-License-Identifier: BSD-4-Clause
    // SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
    //
    // Derived from DragonFly BSD
    // Upstream-Path: sys/platform/pc64/x86_64/machdep.c
    // Upstream-Commit: <upstream commit hash>
    // SPDX-License-Identifier: BSD-4-Clause
    // SPDX-FileCopyrightText: 1982, 1987, 1990 The Regents of the University of California
    // SPDX-FileCopyrightText: 1992 Terrence R. Lambert
    // SPDX-FileCopyrightText: 2003 Peter Wemm.
    // SPDX-FileCopyrightText: 2008-2017 The DragonFly Project

- Upstream lines are copied verbatim, years in the upstream's own form
  (`1991, 1990, 1989, ...` stays as written).
- Every identifier names a text in `LICENCES/` — `BSD-2-Clause` →
  `BSD-2-Clause.txt`, `MIT` → `MIT.txt`. Licences that both apply join with
  ` AND ` on the one identifier line.
- `MIT` is the licence for new code.
- A file that is only declarations -- type aliases, constants, no logic --
  is ours: our two lines only, no upstream lines.
- The provenance block sits below the copyright header, after a bare `//`
  line. `Upstream-Path` is relative to the upstream tree root;
  `Upstream-Commit` pins the revision; the source's SPDX licence and
  copyright lines close the block, licence text never copied. Each further
  source repeats the whole block, and the top identifier joins the source
  licences with ` AND `.
- Files that cannot carry a notice (JSON, data) take no header.
- A `mod.rs` that only declares `pub mod` takes no header either: there is
  nothing in it to license. Declarations of that kind are preferred in
  `mod.rs`, so the exemption is deliberate; as soon as the file holds
  anything else, the header comes back.
