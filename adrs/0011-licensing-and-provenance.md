# Licensing and provenance

New code uses MIT; ported code keeps its sources' licences. Attribution and
upstream references live in the source header, so provenance stays
traceable without dictating the Rust documentation
([ADR 0010](0010-source-conventions.md)).

## Licence header

Every Rust source file, build scripts included, opens with its licences,
then its copyright holders, one per line. The header sits above the `//!`
module doc, so line 1 of every `.rs` file is `// SPDX-License-Identifier:`.
A file written here carries:

    // SPDX-License-Identifier: MIT
    // SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira

A file carrying ported code takes its sources' licences, not MIT. Its own
line is a copyright only; each source adds a provenance block below it,
after a bare `//` line:

    // SPDX-License-Identifier: BSD-4-Clause
    // SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
    //
    // Derived from DragonFly BSD
    // Upstream-Path: sys/platform/pc64/x86_64/machdep.c
    // Upstream-Commit: <upstream commit hash>
    // SPDX-License-Identifier: BSD-4-Clause
    // SPDX-FileCopyrightText: 1982, 1987, 1990 The Regents of the University of California.
    // SPDX-FileCopyrightText: 1992 Terrence R. Lambert.
    // SPDX-FileCopyrightText: 2003 Peter Wemm.
    // SPDX-FileCopyrightText: 2008-2017 The DragonFly Project.

- `Upstream-Path` is relative to the upstream tree root; `Upstream-Commit`
  pins the revision. The source's SPDX licence and copyright lines close
  the block; licence text is never copied.
- Upstream lines are copied verbatim, years in the upstream's own form
  (`1991, 1990, 1989, ...` stays as written). An upstream source without
  its own notice takes the licence and copyright of its project's
  copyright file, such as `dfly/COPYRIGHT`.
- Each further source repeats the whole block. Licences that apply together
  join with ` AND ` on one identifier line, so the top identifier joins the
  source licences.
- Every identifier names a text in `LICENCES/`: `BSD-2-Clause` →
  `BSD-2-Clause.txt`, `MIT` → `MIT.txt`.
- A file of declarations only (type aliases, constants, no logic) is ours:
  our two lines, no upstream lines.
- A `mod.rs` that only declares `pub mod` takes no header: there is nothing
  in it to license. Such declarations are preferred in `mod.rs`, so the
  exemption is deliberate; once the file holds anything else, a module doc
  included, the header returns.
- Rust that replaces imported C follows these rules, with the C file as its
  `Upstream-Path`. Files of the temporary C base follow
  [ADR 0015](0015-dragonfly-c-base.md).
