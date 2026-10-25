// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2026 Leonardo Lopes Pereira
//
// Derived from DragonFly BSD
// Upstream-Path: sys/conf/kern.fwd.mk
// Upstream-Commit: 166068f391a717c3719fe5f88f76f7aeacfbb40e
// SPDX-License-Identifier: BSD-3-Clause
// SPDX-FileCopyrightText: 2003-2026 The DragonFly Project
//! Host-side C build support for the kernel build scripts: tool lookup,
//! host-compiler fixups, the machine include tree, and an incremental,
//! parallel compile driver.
//!
//! The kernel is freestanding x86_64 code, so the host's native gcc and
//! binutils can build it; the fixup flags undo the parts of a
//! Linux-targeted toolchain that leak into freestanding output.
//!
//! Commands run from the C tree root, so paths in diagnostics and
//! dependency files stay relative to it.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::SystemTime;

/// The host compiler targets Linux: undo its OS predefines and claim
/// DragonFly, which the imported headers select code on; turn off code
/// generation the imported code's toolchain did not do by default; and keep
/// newer gcc hard errors as warnings, since the imported code predates them.
pub const HOST_FIXUP_CFLAGS: &[&str] = &[
    "-D__DragonFly__",
    "-U__linux__",
    "-U__linux",
    "-Ulinux",
    "-U__gnu_linux__",
    "-fcf-protection=none",
    "-fno-stack-clash-protection",
    "-Wa,-mx86-used-note=no",
    "-Wa,--noexecstack",
    "-Wno-error=implicit-function-declaration",
    "-Wno-error=implicit-int",
    "-Wno-error=int-conversion",
    "-Wno-error=incompatible-pointer-types",
    "-Wno-error=return-mismatch",
    "-Wno-error=declaration-missing-parameter-type",
];

/// A host tool: a program plus the arguments it always gets.
#[derive(Clone, Debug)]
pub struct Tool(Vec<String>);

impl Tool {
    /// `$var` from the environment, split on whitespace, or `default`.
    pub fn from_env(var: &str, default: &str) -> Tool {
        let value = std::env::var(var)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| default.to_string());
        Tool(value.split_whitespace().map(String::from).collect())
    }

    pub fn command(&self) -> Command {
        let mut cmd = Command::new(&self.0[0]);
        cmd.args(&self.0[1..]);
        cmd
    }
}

/// Environment variables that override the C build tools.
pub const TOOL_VARS: &[&str] = &["CC", "NM", "AWK"];

pub struct Tools {
    pub cc: Tool,
    pub nm: Tool,
    pub awk: Tool,
}

impl Tools {
    pub fn from_env() -> Tools {
        Tools {
            cc: Tool::from_env("CC", "gcc"),
            nm: Tool::from_env("NM", "nm"),
            awk: Tool::from_env("AWK", default_awk()),
        }
    }
}

/// The imported awk scripts use identifiers ("default") that are keywords
/// in gawk's extended mode.
fn default_awk() -> &'static str {
    let gnu = Command::new("awk")
        .arg("--version")
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|o| o.stdout.starts_with(b"GNU Awk"));
    if gnu { "awk --posix" } else { "awk" }
}

/// Builds the machine include topology in `inc`:
///
/// ```text
/// machine/      -> sys/platform/pc64/include
/// machine_base/ -> sys/platform/pc64
/// cpu/          -> sys/cpu/x86_64/include
/// cpu_base/     -> sys/cpu/x86_64
/// fwd/machine/  forwarders for cpu headers the platform does not override
/// ```
pub fn machine_includes(root: &Path, inc: &Path) -> Result<(), String> {
    let fwd = inc.join("fwd/machine");
    fs::create_dir_all(&fwd).map_err(|e| format!("{}: {e}", fwd.display()))?;
    for (link, dir) in [
        ("machine", "sys/platform/pc64/include"),
        ("machine_base", "sys/platform/pc64"),
        ("cpu", "sys/cpu/x86_64/include"),
        ("cpu_base", "sys/cpu/x86_64"),
    ] {
        symlink(&root.join(dir), &inc.join(link))?;
    }
    let cpu = root.join("sys/cpu/x86_64/include");
    for entry in fs::read_dir(&cpu).map_err(|e| format!("{}: {e}", cpu.display()))? {
        let name = entry.map_err(|e| e.to_string())?.file_name();
        let Some(name) = name.to_str().filter(|n| n.ends_with(".h")) else {
            continue;
        };
        let guard = format!("_MACHINE_{}_", name.to_uppercase().replace('.', "_"));
        write_if_changed(
            &fwd.join(name),
            &format!(
                "/*\n * GENERATED FILE, DO NOT EDIT\n */\n\n\
                 #ifndef {guard}\n#define {guard}\n#include <cpu/{name}>\n#endif\n"
            ),
        )?;
    }
    Ok(())
}

fn symlink(target: &Path, link: &Path) -> Result<(), String> {
    if fs::read_link(link).is_ok_and(|t| t == target) {
        return Ok(());
    }
    let _ = fs::remove_file(link);
    std::os::unix::fs::symlink(target, link).map_err(|e| format!("{}: {e}", link.display()))
}

/// One compile: `cc -c <args> -MD -MP -o <obj> <src>`.
#[derive(Clone)]
pub struct Job {
    pub src: PathBuf,
    pub obj: PathBuf,
    pub args: Vec<String>,
}

/// What `compile` did.
pub struct Compiled {
    /// Compiler output of the jobs that succeeded with diagnostics.
    pub warnings: Vec<(PathBuf, String)>,
}

/// Runs the jobs that are out of date, up to `parallel` at a time.
///
/// A job is up to date when its object is newer than every input listed in
/// its dependency file (written by `-MD`) and was built by the same command
/// line, which is recorded next to the object in a `.cmd` file.
pub fn compile(cc: &Tool, root: &Path, jobs: &[Job], parallel: usize) -> Result<Compiled, String> {
    let todo: Vec<&Job> = jobs.iter().filter(|j| !up_to_date(cc, root, j)).collect();
    let next = AtomicUsize::new(0);
    let failed = AtomicBool::new(false);
    let warnings = Mutex::new(Vec::new());
    let errors = Mutex::new(Vec::new());
    std::thread::scope(|s| {
        for _ in 0..parallel.clamp(1, todo.len().max(1)) {
            s.spawn(|| {
                while !failed.load(Ordering::Relaxed) {
                    let Some(job) = todo.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    match run_job(cc, root, job) {
                        Ok(out) if out.is_empty() => {}
                        Ok(out) => warnings.lock().unwrap().push((job.src.clone(), out)),
                        Err(e) => {
                            failed.store(true, Ordering::Relaxed);
                            errors.lock().unwrap().push(e);
                        }
                    }
                }
            });
        }
    });
    let errors = errors.into_inner().unwrap();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    let mut warnings = warnings.into_inner().unwrap();
    warnings.sort();
    Ok(Compiled { warnings })
}

fn command_line(cc: &Tool, job: &Job) -> Vec<String> {
    let mut argv = cc.0.clone();
    argv.push("-c".into());
    argv.extend(job.args.iter().cloned());
    argv.extend(["-MD", "-MP", "-o"].map(String::from));
    argv.push(job.obj.display().to_string());
    argv.push(job.src.display().to_string());
    argv
}

fn cmd_file(target: &Path) -> PathBuf {
    let mut name = target.as_os_str().to_owned();
    name.push(".cmd");
    PathBuf::from(name)
}

fn up_to_date(cc: &Tool, root: &Path, job: &Job) -> bool {
    let Some(built) = mtime(&job.obj) else {
        return false;
    };
    if fs::read_to_string(cmd_file(&job.obj)).ok() != Some(command_line(cc, job).join("\n")) {
        return false;
    }
    depfile_inputs(&job.obj.with_extension("d")).is_some_and(|deps| {
        deps.iter()
            .all(|d| mtime(&root.join(d)).is_some_and(|m| m <= built))
    })
}

fn run_job(cc: &Tool, root: &Path, job: &Job) -> Result<String, String> {
    eprintln!("  CC  {}", job.src.display());
    if let Some(dir) = job.obj.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let argv = command_line(cc, job);
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(root)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("{}: {e}", argv[0]))?;
    let diagnostics = String::from_utf8_lossy(&out.stderr).into_owned();
    if !out.status.success() {
        let _ = fs::remove_file(&job.obj);
        return Err(format!("{}\n{diagnostics}", argv.join(" ")));
    }
    let cmd = cmd_file(&job.obj);
    fs::write(&cmd, argv.join("\n")).map_err(|e| format!("{}: {e}", cmd.display()))?;
    Ok(diagnostics)
}

/// Inputs of the first rule in a make-style dependency file, as written by
/// `gcc -MD`.
fn depfile_inputs(path: &Path) -> Option<Vec<PathBuf>> {
    let text = fs::read_to_string(path).ok()?;
    let mut rule = String::new();
    for line in text.lines() {
        match line.strip_suffix('\\') {
            Some(part) => {
                rule.push_str(part);
                rule.push(' ');
            }
            None => {
                rule.push_str(line);
                break;
            }
        }
    }
    let (_, inputs) = rule.split_once(':')?;
    Some(inputs.split_whitespace().map(PathBuf::from).collect())
}

fn mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// True when `target` is missing or older than any of `inputs`.
pub fn outdated<P: AsRef<Path>>(target: &Path, inputs: impl IntoIterator<Item = P>) -> bool {
    let Some(built) = mtime(target) else {
        return true;
    };
    inputs
        .into_iter()
        .any(|i| mtime(i.as_ref()).is_none_or(|m| m > built))
}

/// Writes `contents` to `path` unless it already holds them, so unchanged
/// generated files keep their modification time. Returns whether it wrote.
pub fn write_if_changed(path: &Path, contents: &str) -> Result<bool, String> {
    if fs::read(path).is_ok_and(|old| old == contents.as_bytes()) {
        return Ok(false);
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(path, contents).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(true)
}

/// Runs `cmd`, with its standard output sent to our standard error: a build
/// script's standard output is reserved for cargo directives.
pub fn run(cmd: &mut Command) -> Result<(), String> {
    let status = cmd
        .stdout(io::stderr())
        .status()
        .map_err(|e| format!("{}: {e}", describe(cmd)))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{} failed ({status})", describe(cmd)))
    }
}

/// Runs `cmd` and returns its standard output.
pub fn output(cmd: &mut Command) -> Result<String, String> {
    let out = cmd
        .stderr(Stdio::inherit())
        .output()
        .map_err(|e| format!("{}: {e}", describe(cmd)))?;
    if !out.status.success() {
        return Err(format!("{} failed ({})", describe(cmd), out.status));
    }
    String::from_utf8(out.stdout).map_err(|e| format!("{}: {e}", describe(cmd)))
}

fn describe(cmd: &Command) -> String {
    std::iter::once(cmd.get_program())
        .chain(cmd.get_args())
        .map(OsStr::to_string_lossy)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Lines of a list file, without `#` comments and blank lines.
pub fn read_list(path: &Path) -> Result<Vec<String>, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(text
        .lines()
        .map(|l| l.split('#').next().unwrap().trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
}

/// Number of parallel compiles: cargo's `NUM_JOBS` in a build script,
/// otherwise the number of CPUs.
pub fn parallelism() -> usize {
    std::env::var("NUM_JOBS")
        .ok()
        .and_then(|n| n.parse().ok())
        .or_else(|| std::thread::available_parallelism().ok().map(|n| n.get()))
        .unwrap_or(1)
}

/// The fields stamped into a kernel build: `date` output, user and host
/// name.
pub struct Stamp {
    pub date: String,
    pub year: String,
    pub user: String,
    pub host: String,
}

impl Stamp {
    pub fn now() -> Result<Stamp, String> {
        let date = |fmt: Option<&str>| {
            output(Command::new("date").args(fmt)).map(|s| s.trim().to_string())
        };
        let host = fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|h| h.trim().to_string())
            .unwrap_or_else(|_| "localhost".into());
        Ok(Stamp {
            date: date(None)?,
            year: date(Some("+%Y"))?,
            user: std::env::var("USER").unwrap_or_else(|_| "root".into()),
            host,
        })
    }
}
