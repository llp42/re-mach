// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from DragonFly BSD
// Upstream-Path: sys/conf/kern.pre.mk
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2003-2026 The DragonFly Project
//
// Derived from DragonFly BSD
// Upstream-Path: sys/conf/bsd.kern.mk
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2003-2026 The DragonFly Project
//
// Derived from DragonFly BSD
// Upstream-Path: sys/platform/pc64/conf/kern.mk
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2003-2026 The DragonFly Project
//
// Derived from DragonFly BSD
// Upstream-Path: sys/kern/genassym.sh
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2003-2026 The DragonFly Project
//
// Derived from DragonFly BSD
// Upstream-Path: sys/conf/newvers.sh
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 1984, 1986, 1990, 1993 The Regents of the University of California
//! Builds the imported C kernel and tells rustc how to link it, and builds
//! the init program.
//!
//! The C tree is `dfly/`; `dfly/files.txt` lists its sources for the
//! DFLYMINI configuration in link order, and `dfly/conf/kernel/` holds the
//! option headers and device table generated for that configuration.
//!
//! rustc then links the kernel with `rust-lld`: this crate first, then the
//! C objects, under the kernel's linker script (see `link_args`).
//!
//! The init program in `tests/init/` is another executable for the same
//! target with other code generation. Stable Cargo cannot build it as part
//! of this crate, so a nested Cargo builds it inside its directory, where
//! its configuration selects its own flags and target directory; the
//! separate target directory keeps it off this build's lock.

use cbuild::{Job, Tool, Tools};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const TARGET: &str = "x86_64-unknown-none";
const IDENT: &str = "DFLYMINI";
const OSRELEASE: &str = "6.5-DEVELOPMENT";

/// Linked first, before the sources in files.txt.
const LOCORE: &str = "sys/platform/pc64/x86_64/locore.s";
/// Not linked; compiled only to extract structure offsets into assym.s.
const GENASSYM: &str = "sys/platform/pc64/x86_64/genassym.c";
const IOCONF: &str = "conf/kernel/ioconf.c";
const MAKEOBJOPS: &str = "sys/tools/makeobjops.awk";
const PARAM_H: &str = "sys/sys/param.h";
/// The init program: crate directory, and its executable as configured in
/// its .cargo/config.toml.
const INIT_CRATE: &str = "tests/init";
const INIT_EXECUTABLE: &str = "target/tests/x86_64-unknown-none";

/// Warning flags of the kernel's C code, without -Werror.
const CWARNFLAGS: &[&str] = &[
    "-Wall",
    "-Wredundant-decls",
    "-Wnested-externs",
    "-Wstrict-prototypes",
    "-Wmissing-prototypes",
    "-Wpointer-arith",
    "-Wcast-qual",
    "-Wold-style-definition",
    "-Wmissing-include-dirs",
    "-Winit-self",
    "-Wundef",
    "-Wold-style-declaration",
    "-Wno-pointer-sign",
    "-Wno-unused-but-set-variable",
    "-Wno-address-of-packed-member",
    "-Wno-zero-length-bounds",
];

/// x86_64 kernel code generation: no FPU/SIMD, kernel code model,
/// retpolines.
const MDFLAGS: &[&str] = &[
    "-fno-stack-protector",
    "-fno-strict-aliasing",
    "-fno-strict-overflow",
    "-fno-asynchronous-unwind-tables",
    "-fno-omit-frame-pointer",
    "-mcmodel=kernel",
    "-mno-red-zone",
    "-mno-mmx",
    "-mno-sse",
    "-msoft-float",
    "-mno-fp-ret-in-387",
    "-mindirect-branch=thunk-inline",
    "-fno-pie",
];

fn main() {
    if let Err(e) = build() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

fn build() -> Result<(), String> {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = manifest.join("dfly");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let glue = manifest.join("src/glue.c");
    let files = root.join("files.txt");
    let ldscript = manifest.join("link/x86_64.ld");

    if env::var("TARGET").as_deref() != Ok(TARGET) {
        return Err(format!(
            "the kernel builds for {TARGET}; run cargo in the repository root \
             so .cargo/config.toml applies"
        ));
    }
    for path in [
        root.join("sys"),
        root.join("conf/kernel"),
        files.clone(),
        glue.clone(),
        ldscript.clone(),
        manifest.join(INIT_CRATE),
    ] {
        println!("cargo::rerun-if-changed={}", path.display());
    }
    for var in cbuild::TOOL_VARS
        .iter()
        .chain(&["KERNEL_COPTFLAGS", "KERNEL_DEBUG", "READELF"])
    {
        println!("cargo::rerun-if-env-changed={var}");
    }

    let tools = Tools::from_env();
    let inc = out.join("inc");
    let gen_dir = out.join("gen");
    let obj = out.join("obj");
    let parallel = cbuild::parallelism();
    cbuild::machine_includes(&root, &inc)?;

    let srcs = cbuild::read_list(&files)?;
    let mfiles: Vec<&str> = srcs
        .iter()
        .map(String::as_str)
        .filter(|s| s.ends_with(".m"))
        .collect();
    for m in &mfiles {
        interface(&tools.awk, &root, &gen_dir, m)?;
    }

    let cflags = cflags(&inc, &gen_dir);
    let mut asflags: Vec<String> = ["-x", "assembler-with-cpp", "-DLOCORE"]
        .map(String::from)
        .into();
    asflags.extend(cflags.iter().cloned());

    // genassym.c defines its symbols as COMMON so nm reports their values.
    let genassym_o = obj.join("genassym.o");
    let mut genassym_flags: Vec<String> = cflags
        .iter()
        .filter(|f| *f != "-fno-common")
        .cloned()
        .collect();
    genassym_flags.push("-fcommon".into());
    let genassym_job = Job {
        src: GENASSYM.into(),
        obj: genassym_o.clone(),
        args: genassym_flags,
    };
    let mut warnings = cbuild::compile(&tools.cc, &root, &[genassym_job], 1)?.warnings;
    cbuild::write_if_changed(&gen_dir.join("assym.s"), &assym(&tools.nm, &genassym_o)?)?;

    // Objects in link order; .m sources compile from their generated .c.
    let mut jobs = Vec::new();
    for src in std::iter::once(LOCORE)
        .chain(srcs.iter().map(String::as_str))
        .chain([IOCONF])
    {
        let job = match src.strip_suffix(".m") {
            Some(m) => {
                let name = Path::new(m).file_name().unwrap().to_str().unwrap();
                Job {
                    src: gen_dir.join(format!("{name}.c")),
                    obj: gen_dir.join(format!("{name}.o")),
                    args: cflags.clone(),
                }
            }
            None => Job {
                src: src.into(),
                obj: obj.join(src).with_extension("o"),
                args: if src.ends_with(".s") || src.ends_with(".S") {
                    asflags.clone()
                } else {
                    cflags.clone()
                },
            },
        };
        jobs.push(job);
    }
    jobs.push(Job {
        src: glue,
        obj: out.join("glue.o"),
        args: cflags.clone(),
    });
    warnings.extend(cbuild::compile(&tools.cc, &root, &jobs, parallel)?.warnings);

    // Regenerated on every relink, so the version string dates the build.
    let vers_c = out.join("vers.c");
    cbuild::write_if_changed(&vers_c, &vers(&root)?)?;
    let vers = Job {
        src: vers_c,
        obj: out.join("vers.o"),
        args: cflags.clone(),
    };
    warnings.extend(cbuild::compile(&tools.cc, &root, std::slice::from_ref(&vers), 1)?.warnings);

    // An empty shared object in DT_NEEDED keeps the kernel a dynamic
    // executable with PT_DYNAMIC, which the kernel linker reads through
    // _DYNAMIC.
    let hack = out.join("hack.So");
    if !hack.exists() {
        let empty = out.join("hack.o");
        cbuild::run(
            tools
                .cc
                .command()
                .args(["-c", "-x", "c", "/dev/null", "-o"])
                .arg(&empty),
        )?;
        cbuild::run(
            Command::new(rust_lld()?)
                .args(["-flavor", "gnu", "-shared", "-soname", "hack.So", "-o"])
                .arg(&hack)
                .arg(&empty),
        )?;
    }

    let mut objs: Vec<PathBuf> = jobs.into_iter().map(|j| j.obj).collect();
    objs.push(hack);
    objs.push(vers.obj);
    for arg in link_args(&root, &ldscript, &objs) {
        println!("cargo::rustc-link-arg-bins={arg}");
    }
    build_init(&manifest)?;

    if !warnings.is_empty() {
        let log = out.join("c-warnings.log");
        let text: String = warnings
            .iter()
            .map(|(src, w)| format!("{}:\n{w}\n", src.display()))
            .collect();
        fs::write(&log, text).map_err(|e| format!("{}: {e}", log.display()))?;
        println!(
            "cargo::warning={} C files compiled with warnings, see {}",
            warnings.len(),
            log.display()
        );
    }
    Ok(())
}

fn cflags(inc: &Path, gen_dir: &Path) -> Vec<String> {
    let env_flags = |var, default: &str| {
        let value = env::var(var).unwrap_or_else(|_| default.into());
        value
            .split_whitespace()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    let mut flags = env_flags("KERNEL_COPTFLAGS", "-O2 -pipe");
    flags.extend(CWARNFLAGS.iter().map(|f| f.to_string()));
    flags.push("-std=gnu11".into());
    flags.extend(env_flags("KERNEL_DEBUG", "-g"));
    flags.extend(["-fno-common", "-ffreestanding"].map(String::from));
    flags.extend(MDFLAGS.iter().map(|f| f.to_string()));
    flags.extend(cbuild::HOST_FIXUP_CFLAGS.iter().map(|f| f.to_string()));
    flags.push("-nostdinc".into());
    flags.push(format!("-I{}", gen_dir.display()));
    flags.push("-Iconf/kernel".into());
    flags.push(format!("-I{}", inc.display()));
    flags.push(format!("-I{}/fwd", inc.display()));
    flags.extend(
        [
            "-Isys",
            "-Isys/dev/acpica",
            "-Isys/contrib/dev/acpica/source/include",
        ]
        .map(String::from),
    );
    flags.extend(
        [
            "-D_KERNEL",
            "-DHAVE_KERNEL_OPTION_HEADERS",
            "-include",
            "opt_global.h",
        ]
        .map(String::from),
    );
    flags
}

/// `rust-lld` from the toolchain that runs this build.
fn rust_lld() -> Result<PathBuf, String> {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let sysroot = cbuild::output(Command::new(rustc).args(["--print", "sysroot"]))?;
    let host = env::var("HOST").unwrap();
    Ok(Path::new(sysroot.trim()).join(format!("lib/rustlib/{host}/bin/rust-lld")))
}

/// The kernel's link options, plus overrides for what rustc asks of the
/// linker. Link arguments come after rustc's own on the command line, so
/// these win.
fn link_args(root: &Path, ldscript: &Path, objs: &[PathBuf]) -> Vec<String> {
    let mut args: Vec<String> = [
        "-z",
        "max-page-size=0x200000",
        "--build-id=sha1",
        "--export-dynamic",
        // rustc links static executables with -static, which refuses the
        // shared object below.
        "-Bdynamic",
    ]
    .map(String::from)
    .into();
    // -L for the debug-info script the linker script includes.
    args.extend(["-L".into(), root.join("sys/conf").display().to_string()]);
    args.extend(["-T".into(), ldscript.display().to_string()]);
    args.extend(
        [
            "--dynamic-linker=/red/herring",
            "-z",
            "norelro",
            "-z",
            "noseparate-code",
            "-z",
            "noexecstack",
            "--hash-style=sysv",
        ]
        .map(String::from),
    );
    // rustc passes --gc-sections and --as-needed. Linker sets are only
    // referenced through __start_/__stop_ symbols, and hack.So has to stay
    // in DT_NEEDED.
    args.extend(["--no-gc-sections", "--no-as-needed"].map(String::from));
    // The prebuilt core and compiler_builtins are PIC and call through the
    // GOT. Exported, their GOT slots would be left to a dynamic linker the
    // kernel does not have; unexported, the linker fills them in.
    args.push("--exclude-libs=ALL".into());
    args.extend(objs.iter().map(|o| o.display().to_string()));
    args
}

/// Builds the init program with a nested Cargo. Its executable must have no
/// dynamic relocations: nothing at run time would apply them.
fn build_init(manifest: &Path) -> Result<(), String> {
    let crate_dir = manifest.join(INIT_CRATE);
    let profile = env::var("PROFILE").unwrap();
    let mut cargo = Command::new(env::var("CARGO").unwrap());
    cargo
        .arg("build")
        .arg("--manifest-path")
        .arg(crate_dir.join("Cargo.toml"))
        .current_dir(&crate_dir);
    if profile == "release" {
        cargo.arg("--release");
    }
    // Settings Cargo hands this script for the kernel would override the
    // init program's own configuration.
    for var in [
        "CARGO_ENCODED_RUSTFLAGS",
        "RUSTFLAGS",
        "CARGO_BUILD_TARGET",
        "CARGO_TARGET_DIR",
        "CARGO_BUILD_TARGET_DIR",
    ] {
        cargo.env_remove(var);
    }
    cbuild::run(&mut cargo)?;

    let executable = manifest.join(INIT_EXECUTABLE).join(profile).join("init");
    let relocations = cbuild::output(
        Tool::from_env("READELF", "readelf")
            .command()
            .arg("-rW")
            .arg(&executable),
    )?;
    if relocations.contains("R_X86_64_") {
        return Err(format!(
            "{}: dynamic relocations, which nothing applies at run time",
            executable.display()
        ));
    }
    Ok(())
}

/// Generates `<name>.h` and `<name>.c` in `gen_dir` from the object
/// interface `m`.
fn interface(awk: &Tool, root: &Path, gen_dir: &Path, m: &str) -> Result<(), String> {
    let src = root.join(m);
    let script = root.join(MAKEOBJOPS);
    let name = Path::new(m).file_stem().unwrap().to_str().unwrap();
    fs::create_dir_all(gen_dir).map_err(|e| format!("{}: {e}", gen_dir.display()))?;
    for (flag, ext) in [("-h", "h"), ("-c", "c")] {
        if cbuild::outdated(&gen_dir.join(format!("{name}.{ext}")), [&src, &script]) {
            cbuild::run(
                awk.command()
                    .arg("-f")
                    .arg(&script)
                    .arg("--")
                    .arg(flag)
                    .arg(&src)
                    .current_dir(gen_dir),
            )?;
        }
    }
    Ok(())
}

/// assym.s from `nm genassym.o`. genassym.c encodes each value as COMMON
/// symbols <name>sign and <name>w0..w3, whose sizes carry the sign and four
/// 16-bit words.
fn assym(nm: &Tool, obj: &Path) -> Result<String, String> {
    let symbols = cbuild::output(nm.command().arg(obj))?;
    let mut out = String::new();
    let mut sign = "";
    let mut words = [""; 4];
    for line in symbols.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        let [value, "C", name, ..] = fields[..] else {
            continue;
        };
        let low = &value[value.len().saturating_sub(4)..];
        if name.ends_with("sign") {
            sign = if low.trim_start_matches('0').is_empty() {
                ""
            } else {
                "-"
            };
        } else if let Some(i) = ["w0", "w1", "w2", "w3"]
            .iter()
            .position(|w| name.ends_with(w))
        {
            words[i] = low;
            if i == 3 {
                let digits = words.iter().rev().copied().collect::<String>();
                let digits = digits.trim_start_matches('0');
                let (hex, digits) = if digits.is_empty() {
                    ("", "0")
                } else {
                    ("0x", digits)
                };
                let name = &name[..name.len() - 2];
                out.push_str(&format!("#define\t{name}\t{sign}{hex}{digits}\n"));
            }
        }
    }
    Ok(out)
}

fn vers(root: &Path) -> Result<String, String> {
    let param = fs::read_to_string(root.join(PARAM_H)).map_err(|e| format!("{PARAM_H}: {e}"))?;
    let reldate = param
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>())
        .find_map(|f| match f[..] {
            [define, "__DragonFly_version", value, ..] if define.ends_with("define") => {
                Some(value.to_string())
            }
            _ => None,
        })
        .ok_or(format!("{PARAM_H}: no __DragonFly_version"))?;
    let rev = Command::new("git")
        .args(["describe", "--always", "--dirty"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|r| !r.is_empty())
        .unwrap_or_else(|| "0".into());
    let stamp = cbuild::Stamp::now()?;
    Ok(format!(
        "/*\n * Copyright (c) 2003-{year} The DragonFly Project.  All rights reserved.\n\
         \x20* See COPYRIGHT for the full copyright notice.\n */\n\n\
         char version[] = \"DragonFly {OSRELEASE} #{rev}: {date}\\n    {user}@{host}:{root}\\n\";\n\
         char ostype[] = \"DragonFly\";\n\
         char osrelease[] = \"{OSRELEASE}\";\n\
         int osreldate = {reldate};\n\
         char kern_ident[] = \"{IDENT}\";\n",
        year = stamp.year,
        date = stamp.date,
        user = stamp.user,
        host = stamp.host,
        root = root.display(),
    ))
}
